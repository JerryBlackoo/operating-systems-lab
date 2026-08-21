//! mkfs - FAT32 文件系统镜像生成工具
//!
//! 用于创建 RustMicroOS 的 FAT32 文件系统镜像
//!
//! 功能：
//! - 创建指定大小的空白 FAT32 镜像
//! - 将用户程序 ELF 文件复制到镜像根目录
//! - 验证镜像格式正确性

use clap::{Parser, Subcommand};
use fatfs::{FileSystem, FormatVolumeOptions, FsOptions, FatType};
use fscommon::BufStream;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::PathBuf;

/// FAT32 文件系统镜像生成工具
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// 创建新的 FAT32 镜像
    Create {
        /// 输出镜像文件路径
        #[arg(short, long)]
        output: PathBuf,

        /// 镜像大小（MB），默认 64MB
        #[arg(short, long, default_value = "64")]
        size: u64,
    },

    /// 向镜像添加文件
    Add {
        /// 镜像文件路径
        #[arg(short, long)]
        image: PathBuf,

        /// 要添加的源文件
        #[arg(short, long)]
        source: PathBuf,

        /// 目标文件名（在镜像中的名称）
        #[arg(short, long)]
        dest: Option<String>,
    },

    /// 列出镜像中的文件
    List {
        /// 镜像文件路径
        #[arg(short, long)]
        image: PathBuf,
    },

    /// 从目录批量添加文件
    Pack {
        /// 输出镜像文件路径
        #[arg(short, long)]
        output: PathBuf,

        /// 输入目录（包含要打包的文件）
        #[arg(short, long)]
        input: PathBuf,

        /// 镜像大小（MB），默认 64MB
        #[arg(short, long, default_value = "64")]
        size: u64,
    },

    /// 添加用户程序 ELF 文件到镜像（从编译输出目录）
    AddElf {
        /// 镜像文件路径
        #[arg(short, long)]
        image: PathBuf,

        /// 用户程序编译输出目录（target/riscv64gc-unknown-none-elf/release）
        #[arg(short, long)]
        elf_dir: PathBuf,

        /// 要添加的程序名称列表（逗号分隔），如果不指定则添加所有 ELF 文件
        #[arg(short, long)]
        programs: Option<String>,
    },

    /// 验证镜像中是否包含必需的文件
    Verify {
        /// 镜像文件路径
        #[arg(short, long)]
        image: PathBuf,
    },
}

/// FAT32 镜像构建器
pub struct DiskImageBuilder {
    image_path: PathBuf,
    size_mb: u64,
}

impl DiskImageBuilder {
    /// 创建新的镜像构建器
    pub fn new(image_path: PathBuf, size_mb: u64) -> Self {
        Self { image_path, size_mb }
    }

    /// 创建空白 FAT32 镜像
    pub fn create(&self) -> io::Result<()> {
        let image_size = self.size_mb * 1024 * 1024;
        
        println!("Creating FAT32 disk image...");
        println!("  Path: {:?}", self.image_path);
        println!("  Size: {} MB ({} bytes)", self.size_mb, image_size);

        // 1. 创建空白镜像文件
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&self.image_path)?;
        
        file.set_len(image_size)?;
        
        // 2. 格式化为 FAT32
        // 使用 BufStream 包装文件，确保数据正确写入
        let buf_stream = BufStream::new(file);
        
        // 配置 FAT32 格式化选项
        let format_options = FormatVolumeOptions::new()
            .fat_type(FatType::Fat32)
            .volume_label(*b"RUSTMICROOS");
        
        // 执行格式化
        fatfs::format_volume(buf_stream, format_options)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("FAT32 format error: {:?}", e)))?;
        
        println!("  Formatted as FAT32");
        println!("FAT32 disk image created successfully!");
        Ok(())
    }

    /// 格式化文件为 FAT32（已废弃，合并到 create 中）
    #[allow(dead_code)]
    fn format_fat32(&self, file: &File) -> io::Result<()> {
        let buf_stream = BufStream::new(file);
        
        // 配置 FAT32 格式化选项
        let format_options = FormatVolumeOptions::new()
            .fat_type(FatType::Fat32)
            .volume_label(*b"RUSTMICROOS");
        
        // 执行格式化
        fatfs::format_volume(buf_stream, format_options)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("FAT32 format error: {:?}", e)))?;
        
        println!("  Formatted as FAT32");
        Ok(())
    }

    /// 向镜像添加文件
    pub fn add_file(&self, src: &PathBuf, dest_name: &str) -> io::Result<()> {
        println!("Adding file to image...");
        println!("  Source: {:?}", src);
        println!("  Destination: /{}", dest_name);

        // 读取源文件内容
        let mut src_file = File::open(src)?;
        let mut content = Vec::new();
        src_file.read_to_end(&mut content)?;

        // 打开镜像
        let img_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.image_path)?;
        
        let buf_stream = BufStream::new(img_file);
        let fs = FileSystem::new(buf_stream, FsOptions::new())
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to open FAT32: {:?}", e)))?;
        
        let root_dir = fs.root_dir();
        
        // 创建目标文件并写入内容
        let mut dest_file = root_dir.create_file(dest_name)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to create file: {:?}", e)))?;
        
        dest_file.write_all(&content)?;
        dest_file.flush()?;
        
        println!("  Added {} bytes", content.len());
        Ok(())
    }

    /// 列出镜像中的文件
    pub fn list_files(&self) -> io::Result<Vec<String>> {
        let img_file = OpenOptions::new()
            .read(true)
            .open(&self.image_path)?;
        
        let buf_stream = BufStream::new(img_file);
        let fs = FileSystem::new(buf_stream, FsOptions::new())
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to open FAT32: {:?}", e)))?;
        
        let root_dir = fs.root_dir();
        let mut files = Vec::new();
        
        for entry in root_dir.iter() {
            let entry = entry.map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Read error: {:?}", e)))?;
            let name = entry.file_name();
            if !entry.is_dir() {
                files.push(format!("{} ({} bytes)", name, entry.len()));
            }
        }
        
        Ok(files)
    }

    /// 检查文件是否为有效的 RISC-V ELF 文件
    fn is_valid_riscv_elf(path: &PathBuf) -> io::Result<bool> {
        let mut file = File::open(path)?;
        let mut magic = [0u8; 16];
        
        if file.read(&mut magic)? < 16 {
            return Ok(false);
        }
        
        // 检查 ELF magic number: 0x7F 'E' 'L' 'F'
        if magic[0] != 0x7F || magic[1] != b'E' || magic[2] != b'L' || magic[3] != b'F' {
            return Ok(false);
        }
        
        // 检查 64-bit (EI_CLASS = 2)
        if magic[4] != 2 {
            return Ok(false);
        }
        
        // 检查 little-endian (EI_DATA = 1)
        if magic[5] != 1 {
            return Ok(false);
        }
        
        Ok(true)
    }

    /// 从 ELF 目录添加用户程序到镜像
    /// 
    /// 参数：
    /// - elf_dir: 用户程序编译输出目录
    /// - programs: 要添加的程序名称列表，None 表示添加所有 ELF 文件
    pub fn add_elf_programs(&self, elf_dir: &PathBuf, programs: Option<Vec<String>>) -> io::Result<Vec<String>> {
        println!("Adding ELF programs from {:?}", elf_dir);
        
        if !elf_dir.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("ELF directory not found: {:?}", elf_dir)
            ));
        }

        // 需要排除的文件（内核和库文件）
        let excluded_files = vec!["kernel", "libuser"];

        let mut added_files = Vec::new();
        
        // 遍历目录中的文件
        for entry in fs::read_dir(elf_dir)? {
            let entry = entry?;
            let path = entry.path();
            
            // 跳过目录和非文件
            if !path.is_file() {
                continue;
            }
            
            let file_name = path.file_name()
                .unwrap()
                .to_string_lossy()
                .to_string();
            
            // 跳过带扩展名的文件（如 .d, .rlib 等）
            if file_name.contains('.') {
                continue;
            }
            
            // 跳过排除列表中的文件
            if excluded_files.contains(&file_name.as_str()) {
                println!("  Skipping {} (excluded)", file_name);
                continue;
            }
            
            // 如果指定了程序列表，检查是否在列表中
            if let Some(ref prog_list) = programs {
                if !prog_list.contains(&file_name) {
                    continue;
                }
            }
            
            // 验证是否为有效的 RISC-V ELF 文件
            if !Self::is_valid_riscv_elf(&path)? {
                println!("  Skipping {} (not a valid RISC-V ELF)", file_name);
                continue;
            }
            
            // 添加文件到镜像
            self.add_file(&path, &file_name)?;
            added_files.push(file_name);
        }
        
        Ok(added_files)
    }

    /// 验证镜像中是否包含必需的文件
    /// 
    /// 返回：
    /// - Ok(true): 所有必需文件都存在
    /// - Ok(false): 缺少必需文件
    pub fn verify_required_files(&self) -> io::Result<bool> {
        println!("Verifying required files in image...");
        
        let img_file = OpenOptions::new()
            .read(true)
            .open(&self.image_path)?;
        
        let buf_stream = BufStream::new(img_file);
        let fs = FileSystem::new(buf_stream, FsOptions::new())
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to open FAT32: {:?}", e)))?;
        
        let root_dir = fs.root_dir();
        
        // 必需的文件列表
        let required_files = vec!["init"];
        let mut found_files = std::collections::HashSet::new();
        
        for entry in root_dir.iter() {
            let entry = entry.map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Read error: {:?}", e)))?;
            if !entry.is_dir() {
                let name = entry.file_name().to_lowercase();
                found_files.insert(name);
            }
        }
        
        let mut all_found = true;
        for required in &required_files {
            if found_files.contains(*required) {
                println!("  [OK] /{} found", required);
            } else {
                println!("  [MISSING] /{} NOT found!", required);
                all_found = false;
            }
        }
        
        if all_found {
            println!("All required files are present.");
        } else {
            println!("WARNING: Some required files are missing!");
        }
        
        Ok(all_found)
    }

    /// 检查镜像中是否存在指定文件
    pub fn file_exists(&self, filename: &str) -> io::Result<bool> {
        let img_file = OpenOptions::new()
            .read(true)
            .open(&self.image_path)?;
        
        let buf_stream = BufStream::new(img_file);
        let fs = FileSystem::new(buf_stream, FsOptions::new())
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to open FAT32: {:?}", e)))?;
        
        let root_dir = fs.root_dir();
        let filename_lower = filename.to_lowercase();
        
        for entry in root_dir.iter() {
            let entry = entry.map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Read error: {:?}", e)))?;
            if !entry.is_dir() && entry.file_name().to_lowercase() == filename_lower {
                return Ok(true);
            }
        }
        
        Ok(false)
    }
}

fn main() -> io::Result<()> {
    let args = Args::parse();

    match args.command {
        Commands::Create { output, size } => {
            let builder = DiskImageBuilder::new(output, size);
            builder.create()?;
        }

        Commands::Add { image, source, dest } => {
            let dest_name = dest.unwrap_or_else(|| {
                source.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            });
            let builder = DiskImageBuilder::new(image, 0);
            builder.add_file(&source, &dest_name)?;
        }

        Commands::List { image } => {
            let builder = DiskImageBuilder::new(image, 0);
            let files = builder.list_files()?;
            
            println!("Files in image:");
            if files.is_empty() {
                println!("  (empty)");
            } else {
                for file in &files {
                    println!("  /{}", file);
                }
            }
            println!("Total: {} files", files.len());
        }

        Commands::Pack { output, input, size } => {
            // 创建镜像
            let builder = DiskImageBuilder::new(output.clone(), size);
            builder.create()?;
            
            // 添加目录中的所有文件
            if input.is_dir() {
                for entry in fs::read_dir(&input)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.is_file() {
                        let name = path.file_name()
                            .unwrap()
                            .to_string_lossy()
                            .to_string();
                        builder.add_file(&path, &name)?;
                    }
                }
            }
            
            // 验证必需文件
            let has_init = builder.verify_required_files()?;
            if !has_init {
                eprintln!("ERROR: /init file is required but not found in the image!");
                std::process::exit(1);
            }
            
            // 列出结果
            let files = builder.list_files()?;
            println!("\nPacked {} files into image", files.len());
        }

        Commands::AddElf { image, elf_dir, programs } => {
            let builder = DiskImageBuilder::new(image, 0);
            
            // 解析程序列表
            let prog_list = programs.map(|p| {
                p.split(',')
                    .map(|s| s.trim().to_string())
                    .collect::<Vec<_>>()
            });
            
            let added = builder.add_elf_programs(&elf_dir, prog_list)?;
            
            println!("\nAdded {} ELF programs:", added.len());
            for name in &added {
                println!("  /{}", name);
            }
            
            // 检查 init 是否存在
            if !builder.file_exists("init")? {
                eprintln!("\nWARNING: /init file not found in image!");
                eprintln!("The system requires /init to boot properly.");
            }
        }

        Commands::Verify { image } => {
            let builder = DiskImageBuilder::new(image, 0);
            let valid = builder.verify_required_files()?;
            
            if !valid {
                std::process::exit(1);
            }
        }
    }

    Ok(())
}
