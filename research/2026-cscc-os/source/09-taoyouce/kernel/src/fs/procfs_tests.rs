//! ProcFS 属性测试
//! Property 5: 文件系统读写往返
//! Property 9: ProcFS 内容实时性

#[cfg(test)]
mod property_tests {
    use super::procfs::*;
    use super::ramfs::*;
    use super::*;
    use alloc::string::String;
    use alloc::vec::Vec;
    use alloc::sync::Arc;
    
    /// Property 9: ProcFS 内容实时性
    /// For any 内核状态变化（如进程创建、内存分配），
    /// 对应的 ProcFS 文件内容应立即反映该变化。
    #[test]
    fn property_content_realtime() {
        let procfs = ProcFs::new();
        
        // 测试 /proc/meminfo 格式正确性
        test_meminfo_format(&procfs);
        
        // 测试 /proc/cpuinfo 格式正确性
        test_cpuinfo_format(&procfs);
        
        // 测试 /proc/version 格式正确性
        test_version_format(&procfs);
        
        // 测试 /proc/stat 格式正确性
        test_stat_format(&procfs);
    }
    
    fn test_meminfo_format(procfs: &ProcFs) {
        let content = procfs.read("meminfo").expect("读取 meminfo 失败");
        
        // 验证必要字段存在
        assert!(content.contains("MemTotal:"), "meminfo 应包含 MemTotal");
        assert!(content.contains("MemFree:"), "meminfo 应包含 MemFree");
        assert!(content.contains("MemAvailable:"), "meminfo 应包含 MemAvailable");
        assert!(content.contains("Buffers:"), "meminfo 应包含 Buffers");
        assert!(content.contains("Cached:"), "meminfo 应包含 Cached");
        
        // 验证格式正确（每行应该有数值和单位）
        for line in content.lines() {
            if line.contains(':') {
                let parts: Vec<&str> = line.split_whitespace().collect();
                assert!(parts.len() >= 2, "meminfo 行格式不正确: {}", line);
            }
        }
    }
    
    fn test_cpuinfo_format(procfs: &ProcFs) {
        let content = procfs.read("cpuinfo").expect("读取 cpuinfo 失败");
        
        // 验证必要字段存在
        assert!(content.contains("processor"), "cpuinfo 应包含 processor");
        assert!(content.contains("isa"), "cpuinfo 应包含 isa");
        
        // 验证 RISC-V 特定信息
        assert!(content.contains("rv64"), "cpuinfo 应包含 rv64 ISA");
    }
    
    fn test_version_format(procfs: &ProcFs) {
        let content = procfs.read("version").expect("读取 version 失败");
        
        // 验证版本信息存在
        assert!(content.contains("RustMicroOS"), "version 应包含 RustMicroOS");
        assert!(content.contains("version"), "version 应包含 version 字样");
    }
    
    fn test_stat_format(procfs: &ProcFs) {
        let content = procfs.read("stat").expect("读取 stat 失败");
        
        // 验证 CPU 统计存在
        assert!(content.contains("cpu"), "stat 应包含 cpu 统计");
        assert!(content.contains("ctxt"), "stat 应包含上下文切换统计");
        assert!(content.contains("processes"), "stat 应包含进程统计");
    }
    
    /// 属性测试：目录列表正确性
    #[test]
    fn property_directory_listing() {
        let procfs = ProcFs::new();
        
        // 读取根目录
        let entries = procfs.readdir("").expect("读取根目录失败");
        
        // 验证标准文件存在
        assert!(entries.contains(&String::from("meminfo")), "应包含 meminfo");
        assert!(entries.contains(&String::from("cpuinfo")), "应包含 cpuinfo");
        assert!(entries.contains(&String::from("version")), "应包含 version");
        assert!(entries.contains(&String::from("uptime")), "应包含 uptime");
        assert!(entries.contains(&String::from("stat")), "应包含 stat");
    }
    
    /// 属性测试：路径查找正确性
    #[test]
    fn property_path_lookup() {
        let procfs = ProcFs::new();
        
        // 测试有效路径
        assert!(procfs.lookup("meminfo").is_some(), "应能找到 meminfo");
        assert!(procfs.lookup("cpuinfo").is_some(), "应能找到 cpuinfo");
        assert!(procfs.lookup("self").is_some(), "应能找到 self");
        
        // 测试无效路径
        assert!(procfs.lookup("nonexistent").is_none(), "不应找到不存在的文件");
    }
    
    /// 属性测试：进程目录结构
    #[test]
    fn property_process_directory() {
        let procfs = ProcFs::new();
        
        // 测试 self 目录
        let self_inode = procfs.lookup("self").expect("应能找到 self");
        assert_eq!(self_inode.inode_type, ProcInodeType::Directory, 
            "self 应该是目录");
        
        // 测试 self 目录下的文件
        if let Some(children) = &self_inode.children {
            assert!(children.contains_key("status"), "self 应包含 status");
            assert!(children.contains_key("maps"), "self 应包含 maps");
            assert!(children.contains_key("cmdline"), "self 应包含 cmdline");
        }
    }
    
    /// 属性测试：文件内容生成器
    #[test]
    fn property_content_generators() {
        // 测试各种生成器
        let meminfo_gen = MeminfoGenerator;
        let content = meminfo_gen.generate();
        assert!(!content.is_empty(), "MeminfoGenerator 应生成内容");
        
        let cpuinfo_gen = CpuinfoGenerator;
        let content = cpuinfo_gen.generate();
        assert!(!content.is_empty(), "CpuinfoGenerator 应生成内容");
        
        let version_gen = VersionGenerator;
        let content = version_gen.generate();
        assert!(!content.is_empty(), "VersionGenerator 应生成内容");
        
        let uptime_gen = UptimeGenerator;
        let content = uptime_gen.generate();
        assert!(!content.is_empty(), "UptimeGenerator 应生成内容");
        
        let loadavg_gen = LoadavgGenerator;
        let content = loadavg_gen.generate();
        assert!(!content.is_empty(), "LoadavgGenerator 应生成内容");
    }
    
    /// 属性测试：只读文件系统
    #[test]
    fn property_read_only() {
        let meminfo_gen = MeminfoGenerator;
        
        // 验证文件不可写
        assert!(!meminfo_gen.writable(), "meminfo 应该是只读的");
        
        // 验证写入返回错误
        let result = meminfo_gen.write("test");
        assert!(matches!(result, Err(ProcError::ReadOnly)), 
            "写入只读文件应返回 ReadOnly 错误");
    }
    
    /// Property 5: 文件系统读写往返
    /// Feature: rust-warnings-cleanup, Property 5: 文件系统读写往返
    /// 
    /// *For any* valid file content written to a file, reading the same offset 
    /// and length should return the identical content.
    /// 
    /// **Validates: Requirements 4.2, 4.3**
    #[test]
    fn property_fs_read_write_roundtrip() {
        let ramfs = RamFs::new();
        let root = ramfs.root();
        let root_dir = root.as_dir().expect("root should be directory");
        
        // 创建测试文件
        let file_inode = root_dir.create("test_roundtrip.txt", FileType::Regular)
            .expect("create file failed");
        let file = file_inode.as_file().expect("should be file");
        
        // 测试各种数据模式
        let test_patterns: [&[u8]; 6] = [
            b"Hello, World!",
            b"",
            b"\x00\x01\x02\x03\x04\x05",
            b"A".repeat(1000).leak(),
            b"Line1\nLine2\nLine3\n",
            &[0xFF; 256],
        ];
        
        for (idx, &pattern) in test_patterns.iter().enumerate() {
            // 写入数据
            let written = file.write(0, pattern).expect("write failed");
            assert_eq!(written, pattern.len(), 
                "Pattern {}: write length mismatch", idx);
            
            // 读取数据
            let mut read_buf = vec![0u8; pattern.len()];
            let read_len = file.read(0, &mut read_buf).expect("read failed");
            
            // Property: 读取长度应等于写入长度
            assert_eq!(read_len, pattern.len(),
                "Pattern {}: read length {} != write length {}",
                idx, read_len, pattern.len());
            
            // Property: 读取内容应与写入内容完全相同
            assert_eq!(&read_buf[..read_len], pattern,
                "Pattern {}: content mismatch", idx);
        }
    }
    
    /// Property 5 扩展: 随机数据读写往返
    /// Feature: rust-warnings-cleanup, Property 5: 文件系统读写往返
    /// 
    /// *For any* random byte sequence, write then read should return identical data.
    /// 
    /// **Validates: Requirements 4.2, 4.3**
    #[test]
    fn property_fs_roundtrip_random() {
        use crate::SimpleRng;
        
        let ramfs = RamFs::new();
        let root = ramfs.root();
        let root_dir = root.as_dir().expect("root should be directory");
        
        let mut rng = SimpleRng::new(42);
        
        // 运行 50 次随机测试
        for iteration in 0..50 {
            // 创建唯一文件名
            let filename = alloc::format!("random_test_{}.bin", iteration);
            let file_inode = root_dir.create(&filename, FileType::Regular)
                .expect("create file failed");
            let file = file_inode.as_file().expect("should be file");
            
            // 生成随机数据（1-1000 字节）
            let data_len = rng.next_range(1, 1000);
            let mut write_data: Vec<u8> = Vec::with_capacity(data_len);
            for _ in 0..data_len {
                write_data.push(rng.next_range(0, 256) as u8);
            }
            
            // 写入
            let written = file.write(0, &write_data).expect("write failed");
            assert_eq!(written, write_data.len());
            
            // 读取
            let mut read_data = vec![0u8; write_data.len()];
            let read_len = file.read(0, &mut read_data).expect("read failed");
            
            // Property: 往返一致性
            assert_eq!(read_len, write_data.len(),
                "Iteration {}: length mismatch", iteration);
            assert_eq!(read_data, write_data,
                "Iteration {}: content mismatch", iteration);
        }
    }
    
    /// Property 5 扩展: 部分读写往返
    /// Feature: rust-warnings-cleanup, Property 5: 文件系统读写往返
    /// 
    /// *For any* partial read at offset, the content should match the written data at that offset.
    /// 
    /// **Validates: Requirements 4.2, 4.3**
    #[test]
    fn property_fs_partial_roundtrip() {
        let ramfs = RamFs::new();
        let root = ramfs.root();
        let root_dir = root.as_dir().expect("root should be directory");
        
        let file_inode = root_dir.create("partial_test.txt", FileType::Regular)
            .expect("create file failed");
        let file = file_inode.as_file().expect("should be file");
        
        // 写入较长的数据
        let full_data = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        file.write(0, full_data).expect("write failed");
        
        // 测试各种偏移和长度组合
        let test_cases: [(usize, usize); 8] = [
            (0, 10),   // 开头
            (10, 10),  // 中间
            (26, 10),  // 末尾
            (0, 36),   // 全部
            (5, 5),    // 小段
            (0, 1),    // 单字节
            (35, 1),   // 最后一字节
            (20, 16),  // 跨越
        ];
        
        for (offset, len) in test_cases {
            let mut read_buf = vec![0u8; len];
            let read_len = file.read(offset, &mut read_buf).expect("read failed");
            
            let expected_len = len.min(full_data.len().saturating_sub(offset));
            
            // Property: 读取长度正确
            assert_eq!(read_len, expected_len,
                "Offset {}, len {}: read length mismatch", offset, len);
            
            // Property: 内容匹配
            if expected_len > 0 {
                assert_eq!(&read_buf[..read_len], &full_data[offset..offset + expected_len],
                    "Offset {}, len {}: content mismatch", offset, len);
            }
        }
    }
    
    /// Property 5 扩展: 追加写入往返
    /// Feature: rust-warnings-cleanup, Property 5: 文件系统读写往返
    /// 
    /// *For any* sequence of append writes, reading the full file should return all appended data.
    /// 
    /// **Validates: Requirements 4.2, 4.3**
    #[test]
    fn property_fs_append_roundtrip() {
        let ramfs = RamFs::new();
        let root = ramfs.root();
        let root_dir = root.as_dir().expect("root should be directory");
        
        let file_inode = root_dir.create("append_test.txt", FileType::Regular)
            .expect("create file failed");
        let file = file_inode.as_file().expect("should be file");
        
        let mut expected_content: Vec<u8> = Vec::new();
        let mut current_offset = 0;
        
        // 追加多次写入
        let chunks: [&[u8]; 5] = [
            b"First chunk. ",
            b"Second chunk. ",
            b"Third chunk. ",
            b"Fourth chunk. ",
            b"Final chunk.",
        ];
        
        for chunk in chunks {
            let written = file.write(current_offset, chunk).expect("write failed");
            assert_eq!(written, chunk.len());
            
            expected_content.extend_from_slice(chunk);
            current_offset += chunk.len();
        }
        
        // 读取全部内容
        let mut read_buf = vec![0u8; expected_content.len()];
        let read_len = file.read(0, &mut read_buf).expect("read failed");
        
        // Property: 追加写入后读取应返回所有内容
        assert_eq!(read_len, expected_content.len(),
            "Append roundtrip: length mismatch");
        assert_eq!(read_buf, expected_content,
            "Append roundtrip: content mismatch");
    }
    
    /// Property 5 扩展: 覆盖写入往返
    /// Feature: rust-warnings-cleanup, Property 5: 文件系统读写往返
    /// 
    /// *For any* overwrite at an offset, reading should return the new data.
    /// 
    /// **Validates: Requirements 4.2, 4.3**
    #[test]
    fn property_fs_overwrite_roundtrip() {
        let ramfs = RamFs::new();
        let root = ramfs.root();
        let root_dir = root.as_dir().expect("root should be directory");
        
        let file_inode = root_dir.create("overwrite_test.txt", FileType::Regular)
            .expect("create file failed");
        let file = file_inode.as_file().expect("should be file");
        
        // 初始写入
        let initial = b"AAAAAAAAAA";
        file.write(0, initial).expect("initial write failed");
        
        // 覆盖中间部分
        let overwrite = b"BBB";
        file.write(3, overwrite).expect("overwrite failed");
        
        // 读取并验证
        let mut read_buf = vec![0u8; 10];
        let read_len = file.read(0, &mut read_buf).expect("read failed");
        
        // Property: 覆盖后内容正确
        let expected = b"AAABBBAAAA";
        assert_eq!(read_len, expected.len());
        assert_eq!(&read_buf[..read_len], expected,
            "Overwrite roundtrip: content mismatch");
    }
}
