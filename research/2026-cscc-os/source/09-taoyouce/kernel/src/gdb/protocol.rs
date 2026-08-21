//! GDB RSP 协议实现
//!
//! 实现 GDB Remote Serial Protocol 的数据包编解码

use alloc::vec::Vec;

/// RSP 数据包
#[derive(Debug, Clone)]
pub struct RspPacket {
    /// 数据内容
    pub data: Vec<u8>,
    /// 校验和
    pub checksum: u8,
}

impl RspPacket {
    /// 创建新数据包
    pub fn new(data: Vec<u8>) -> Self {
        let checksum = Self::calculate_checksum(&data);
        Self { data, checksum }
    }

    /// 计算校验和
    pub fn calculate_checksum(data: &[u8]) -> u8 {
        data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b))
    }

    /// 编码为字节流
    pub fn encode(&self) -> Vec<u8> {
        let mut result = Vec::with_capacity(self.data.len() + 4);
        result.push(b'$');
        
        // 转义特殊字符
        for &byte in &self.data {
            match byte {
                b'$' | b'#' | b'}' | b'*' => {
                    result.push(b'}');
                    result.push(byte ^ 0x20);
                }
                _ => result.push(byte),
            }
        }
        
        result.push(b'#');
        result.push(Self::hex_digit(self.checksum >> 4));
        result.push(Self::hex_digit(self.checksum & 0x0f));
        
        result
    }

    /// 从字节流解码
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < 4 {
            return None;
        }

        // 查找 $ 和 #
        let start = data.iter().position(|&b| b == b'$')?;
        let end = data[start..].iter().position(|&b| b == b'#')? + start;

        if end + 2 >= data.len() {
            return None;
        }

        // 提取数据
        let mut packet_data = Vec::new();
        let mut i = start + 1;
        while i < end {
            if data[i] == b'}' && i + 1 < end {
                packet_data.push(data[i + 1] ^ 0x20);
                i += 2;
            } else {
                packet_data.push(data[i]);
                i += 1;
            }
        }

        // 解析校验和
        let checksum_high = Self::parse_hex_digit(data[end + 1])?;
        let checksum_low = Self::parse_hex_digit(data[end + 2])?;
        let checksum = (checksum_high << 4) | checksum_low;

        // 验证校验和
        let calculated = Self::calculate_checksum(&packet_data);
        if calculated != checksum {
            return None;
        }

        Some(Self {
            data: packet_data,
            checksum,
        })
    }

    /// 十六进制数字转字符
    fn hex_digit(n: u8) -> u8 {
        if n < 10 {
            b'0' + n
        } else {
            b'a' + n - 10
        }
    }

    /// 字符转十六进制数字
    fn parse_hex_digit(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
}

/// RSP 响应
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RspResponse {
    /// 确认
    Ack,
    /// 否认
    Nack,
    /// 中断
    Interrupt,
}

impl RspResponse {
    /// 编码为字节
    pub fn encode(&self) -> u8 {
        match self {
            Self::Ack => b'+',
            Self::Nack => b'-',
            Self::Interrupt => 0x03,
        }
    }

    /// 从字节解码
    pub fn decode(byte: u8) -> Option<Self> {
        match byte {
            b'+' => Some(Self::Ack),
            b'-' => Some(Self::Nack),
            0x03 => Some(Self::Interrupt),
            _ => None,
        }
    }
}

/// RSP 状态机
pub struct RspStateMachine {
    /// 当前状态
    state: RspState,
    /// 接收缓冲区
    buffer: Vec<u8>,
    /// 是否启用 ACK 模式
    ack_mode: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RspState {
    /// 等待数据包开始
    Idle,
    /// 接收数据
    ReceivingData,
    /// 接收校验和第一位
    ReceivingChecksum1,
    /// 接收校验和第二位
    ReceivingChecksum2,
}

impl RspStateMachine {
    /// 创建新状态机
    pub fn new() -> Self {
        Self {
            state: RspState::Idle,
            buffer: Vec::new(),
            ack_mode: true,
        }
    }

    /// 处理输入字节
    pub fn process(&mut self, byte: u8) -> Option<RspPacket> {
        match self.state {
            RspState::Idle => {
                if byte == b'$' {
                    self.buffer.clear();
                    self.state = RspState::ReceivingData;
                }
                None
            }
            RspState::ReceivingData => {
                if byte == b'#' {
                    self.state = RspState::ReceivingChecksum1;
                } else {
                    self.buffer.push(byte);
                }
                None
            }
            RspState::ReceivingChecksum1 => {
                self.buffer.push(byte);
                self.state = RspState::ReceivingChecksum2;
                None
            }
            RspState::ReceivingChecksum2 => {
                self.buffer.push(byte);
                self.state = RspState::Idle;
                
                // 解析数据包
                let data_len = self.buffer.len() - 2;
                let data = self.buffer[..data_len].to_vec();
                
                let checksum_high = RspPacket::parse_hex_digit(self.buffer[data_len])?;
                let checksum_low = RspPacket::parse_hex_digit(self.buffer[data_len + 1])?;
                let checksum = (checksum_high << 4) | checksum_low;
                
                let calculated = RspPacket::calculate_checksum(&data);
                if calculated == checksum {
                    Some(RspPacket { data, checksum })
                } else {
                    None
                }
            }
        }
    }

    /// 重置状态机
    pub fn reset(&mut self) {
        self.state = RspState::Idle;
        self.buffer.clear();
    }

    /// 禁用 ACK 模式
    pub fn disable_ack(&mut self) {
        self.ack_mode = false;
    }

    /// 是否启用 ACK 模式
    pub fn ack_enabled(&self) -> bool {
        self.ack_mode
    }
}

impl Default for RspStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packet_encode_decode() {
        let packet = RspPacket::new(b"g".to_vec());
        let encoded = packet.encode();
        let decoded = RspPacket::decode(&encoded).unwrap();
        assert_eq!(decoded.data, b"g");
    }

    #[test]
    fn test_checksum() {
        let data = b"OK";
        let checksum = RspPacket::calculate_checksum(data);
        assert_eq!(checksum, (b'O' as u8).wrapping_add(b'K' as u8));
    }

    #[test]
    fn test_state_machine() {
        let mut sm = RspStateMachine::new();
        
        // 发送 "$g#67"
        assert!(sm.process(b'$').is_none());
        assert!(sm.process(b'g').is_none());
        assert!(sm.process(b'#').is_none());
        assert!(sm.process(b'6').is_none());
        let packet = sm.process(b'7').unwrap();
        assert_eq!(packet.data, b"g");
    }
}
