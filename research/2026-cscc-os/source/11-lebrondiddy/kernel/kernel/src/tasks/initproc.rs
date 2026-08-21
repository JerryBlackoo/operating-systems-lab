#![allow(dead_code)]
#![allow(unused_imports)]
use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use core::time::Duration;
use executor::{
    current_task, purge_exited_user_tasks, release_task, task::TaskType, tid2task, yield_now,
    TASK_MAP,
};
use fs::dentry::umount;
use fs::{file::File, FileType};
use libc_types::fcntl::OpenFlags;
use log::debug;
use polyhal::timer::current_time;
use vfscore::INodeInterface;

use fs::pathbuf::PathBuf;

use crate::watchdog::{
    arm_global_deadline_after, clear_child_task_budget, disarm_global_deadline,
    set_child_task_budget,
};
use crate::{shutdown::shutdown, tasks::add_user_task_with_cwd};

use super::UserTask;

const GLOBAL_DEADLINE_SECS: u64 = 6600;
const REAPER_EXIT_CODE: usize = 137;
const WATCHDOG_EXIT_CODE: usize = 124;
const LTP_CASE_LIMIT_OVERRIDE: &str = "/.ltp_case_limit";

const GROUP_ORDER: [&str; 12] = [
    "basic",
    "busybox",
    "lua",
    "libctest",
    "libcbench",
    "iozone",
    "lmbench",
    "cyclictest",
    "unixbench",
    "iperf",
    "netperf",
    "ltp",
];

const SKIPPED_MARKER_GROUPS: [&str; 4] = [
    "libctest-glibc",
    "unixbench",
    "cyclictest-glibc",
    "cyclictest-musl",
];
const LOCAL_DEADLINE_OVERRIDE: &str = "/.watchdog_global_deadline_secs";
const BUSYBOX_APPLETS: [&str; 58] = [
    "[", "ash", "awk", "basename", "cal", "cat", "chmod", "chown", "clear", "cmp", "cp", "cut",
    "date", "df", "diff", "dirname", "dmesg", "du", "echo", "egrep", "env", "expr", "false",
    "fgrep", "find", "free", "grep", "head", "hexdump", "hwclock", "kill", "ln", "ls", "md5sum",
    "mkdir", "mktemp", "more", "mv", "od", "printf", "ps", "pwd", "readlink", "rm", "rmdir", "sed",
    "sh", "sleep", "sort", "stat", "strings", "tail", "test", "touch", "tr", "true", "uname", "wc",
];

fn read_u64_file(path: &str) -> Option<u64> {
    let file = File::open(path, OpenFlags::RDONLY).ok()?;
    let size = file.file_size().ok()?.min(32);
    if size == 0 {
        return None;
    }
    let mut buffer = vec![0; size];
    let read_size = file.readat(0, &mut buffer).ok()?;
    let text = String::from_utf8_lossy(&buffer[..read_size]);
    text.trim().parse().ok()
}

fn global_deadline_secs() -> u64 {
    read_u64_file(LOCAL_DEADLINE_OVERRIDE).unwrap_or(GLOBAL_DEADLINE_SECS)
}

async fn kill_all_tasks(exit_code: usize, wait_rounds: usize) {
    let current_id = current_task().get_task_id();
    let rounds = wait_rounds.max(1);
    let mut released_total = 0;
    let mut purged_total = 0;
    let mut remaining = 0;

    for round in 0..=rounds {
        let targets: Vec<_> = TASK_MAP
            .lock()
            .iter()
            .filter_map(|(task_id, task)| {
                let task = task.upgrade()?;
                (task.get_task_type() == TaskType::MonolithicTask && *task_id != current_id)
                    .then_some((*task_id, task))
            })
            .collect();

        if targets.is_empty() {
            remaining = 0;
            break;
        }
        remaining = targets.len();

        for (_, task) in targets.iter() {
            if task.exit_code().is_none() {
                task.exit(exit_code);
            }
        }

        let exited: Vec<_> = targets
            .iter()
            .filter_map(|(task_id, task)| task.exit_code().is_some().then_some(*task_id))
            .collect();
        for task_id in exited {
            release_task(task_id);
            released_total += 1;
        }

        if round == rounds {
            break;
        }
        purged_total += purge_exited_user_tasks();
        yield_now().await;
    }
    purged_total += purge_exited_user_tasks();

    if released_total > 0 || purged_total > 0 || remaining > 0 {
        println!(
            "initproc watchdog: reaped {} tasks, purged {} queued tasks, remaining {}",
            released_total, purged_total, remaining
        );
    }
}

#[derive(Clone)]
struct TestScript {
    path: String,
    group: String,
    marker_group: String,
    markers: ScriptMarkers,
    work_dir: PathBuf,
}

#[derive(Clone, Default)]
struct ScriptMarkers {
    start: bool,
    end: bool,
    first_group: Option<String>,
}

enum CommandResult {
    Exited(usize),
    TimedOut,
    Truncated,
    NotFound,
}

async fn command_in_dir(
    work_dir: PathBuf,
    cmd: &str,
    watchdog_name: &str,
    group_budget: Duration,
    task_budget: Duration,
) -> CommandResult {
    let mut args: Vec<&str> = cmd.split(" ").filter(|x| *x != "").collect();
    debug!("cmd: {}  args: {:?}", cmd, args);
    let Some(filename) = args.drain(..1).last() else {
        return CommandResult::NotFound;
    };
    let file_path = work_dir.join(filename);
    match File::open(file_path, OpenFlags::RDONLY) {
        Ok(_) => {
            info!("exec: {}", filename);
            let mut args_extend = vec![filename];
            args_extend.extend(args.into_iter());
            let path_env = if watchdog_name == "ltp-glibc" || watchdog_name == "ltp-musl" {
                format!(
                    "PATH={}:{}:{}:/:/bin:/usr/bin",
                    work_dir.path(),
                    work_dir.join("ltp/testcases/bin").path(),
                    work_dir.join("ltp/testcases/lib").path()
                )
            } else {
                format!("PATH={}:/:/bin:/usr/bin", work_dir.path())
            };
            let lib_env = if work_dir.path() == "/glibc" || work_dir.path().starts_with("/glibc/") {
                String::from("LD_LIBRARY_PATH=/glibc/lib:./lib")
            } else if work_dir.path() == "/musl" || work_dir.path().starts_with("/musl/") {
                String::from("LD_LIBRARY_PATH=/musl/lib:./lib")
            } else {
                String::from("LD_LIBRARY_PATH=./lib")
            };
            let mut envs = vec![path_env.as_str(), lib_env.as_str()];
            if watchdog_name == "ltp-glibc" || watchdog_name == "ltp-musl" {
                envs.push("KCONFIG_PATH=/proc/config");
            }
            clear_child_task_budget();
            let task_id = add_user_task_with_cwd(work_dir, &filename, args_extend, envs).await;
            let Some(task) = tid2task(task_id) else {
                return CommandResult::NotFound;
            };
            let user_task = task.clone().downcast_arc::<UserTask>().ok();
            if let Some(user_task) = &user_task {
                user_task.set_wall_deadline_after(group_budget);
            }
            set_child_task_budget(task_budget, 0);
            let started = current_time();
            let group_deadline = current_time() + group_budget;
            let mut next_heartbeat = current_time() + Duration::from_secs(30);
            let result = loop {
                if user_task
                    .as_ref()
                    .is_some_and(|task| task.wall_deadline_fired())
                {
                    release_task(task_id);
                    break CommandResult::TimedOut;
                }
                if task.exit_code().is_some() {
                    let exit_code = task.exit_code().unwrap_or(0);
                    release_task(task_id);
                    break CommandResult::Exited(exit_code);
                }
                let now = current_time();
                if now >= group_deadline {
                    println!("initproc watchdog: command '{}' timed out", cmd);
                    kill_all_tasks(WATCHDOG_EXIT_CODE, 0).await;
                    release_task(task_id);
                    break CommandResult::TimedOut;
                }
                if now >= next_heartbeat {
                    let elapsed = now
                        .checked_sub(started)
                        .unwrap_or_else(|| Duration::from_secs(0));
                    println!(
                        "initproc watchdog: group {} still running at {}s",
                        watchdog_name,
                        elapsed.as_secs()
                    );
                    next_heartbeat = now + Duration::from_secs(30);
                }
                yield_now().await;
            };
            clear_child_task_budget();
            result
            // syscall(SYS_WAIT4, [0,0,0,0,0,0,0])
            //     .await
            //     .expect("can't wait a pid");
        }
        Err(_) => {
            println!("unknown command: {}", cmd);
            CommandResult::NotFound
        }
    }
}

async fn script_command(script: &TestScript) -> CommandResult {
    let work_dir = script.work_dir.clone();
    prepare_group_rootfs(&work_dir, true);
    let script_path = script.path.as_str();
    let cmd = String::from("busybox sh ") + script_path;
    match command_in_dir(
        work_dir.clone(),
        &cmd,
        &script.marker_group,
        group_budget(&script.group, &script.marker_group),
        task_budget(&script.group, &script.marker_group),
    )
    .await
    {
        CommandResult::NotFound => {
            let fallback = String::from("/busybox sh ") + script_path;
            command_in_dir(
                work_dir,
                &fallback,
                &script.marker_group,
                group_budget(&script.group, &script.marker_group),
                task_budget(&script.group, &script.marker_group),
            )
            .await
        }
        result => result,
    }
}

fn prepare_ltp_rootfs(work_dir: &PathBuf) {
    fs::refresh_runtime_mounts();
    prepare_group_rootfs(work_dir, true);
    let _ = File::open("/var/tmp", OpenFlags::DIRECTORY | OpenFlags::CREAT);
    let _ = File::open("/var/lock", OpenFlags::DIRECTORY | OpenFlags::CREAT);
    let _ = File::open("/run", OpenFlags::DIRECTORY | OpenFlags::CREAT);
    let _ = File::open(
        work_dir.join("tmp"),
        OpenFlags::DIRECTORY | OpenFlags::CREAT,
    );
    let _ = File::open(
        work_dir.join("mntpoint"),
        OpenFlags::DIRECTORY | OpenFlags::CREAT,
    );
    let _ = File::open(
        work_dir.join("mntpoint/ovl"),
        OpenFlags::DIRECTORY | OpenFlags::CREAT,
    );
}

fn clear_ltp_case_scratch(work_dir: &PathBuf) {
    clear_dir("/tmp");
    clear_dir("/dev/shm");
    clear_dir("/var/tmp");
    clear_dir("/var/lock");
    clear_dir("/run");

    let tmp = work_dir.join("tmp").path();
    clear_dir(&tmp);

    let mntpoint = work_dir.join("mntpoint").path();
    clear_dir(&mntpoint);

    let ovl = work_dir.join("mntpoint/ovl").path();
    let _ = File::open(ovl, OpenFlags::DIRECTORY | OpenFlags::CREAT);
}

fn ltp_case_names(work_dir: &PathBuf) -> Vec<String> {
    let Ok(dir) = File::open(work_dir.join("ltp/testcases/bin"), OpenFlags::DIRECTORY) else {
        return Vec::new();
    };
    let Ok(entries) = dir.read_dir() else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .into_iter()
        .filter(|entry| entry.file_type == FileType::File)
        .filter(|entry| !entry.filename.starts_with("."))
        .map(|entry| entry.filename)
        .collect();
    names.sort();
    names
}

fn command_exit_code(result: CommandResult) -> usize {
    match result {
        CommandResult::Exited(code) => code.min(255),
        CommandResult::TimedOut => WATCHDOG_EXIT_CODE,
        CommandResult::Truncated => WATCHDOG_EXIT_CODE,
        CommandResult::NotFound => 127,
    }
}

fn ltp_case_limit(work_dir: &PathBuf) -> Option<usize> {
    read_u64_file(&work_dir.join(".ltp_case_limit").path())
        .or_else(|| read_u64_file(LTP_CASE_LIMIT_OVERRIDE))
        .map(|limit| limit as usize)
        .filter(|limit| *limit > 0)
}

fn ltp_case_quarantined(name: &str) -> bool {
    let name = name.strip_prefix("000_").unwrap_or(name);
    if name.starts_with("gre") && name.ends_with(".sh") {
        return true;
    }
    matches!(
        name,
        "clone02" | "crash01" | "crash02" | "dma_thread_diotest" | "mallocstress"
    )
}

async fn run_ltp_cases(script: &TestScript) -> CommandResult {
    let work_dir = script.work_dir.clone();
    prepare_ltp_rootfs(&work_dir);
    let cases = ltp_case_names(&work_dir);
    let case_limit = ltp_case_limit(&work_dir);
    let group_budget = group_budget(&script.group, &script.marker_group);
    let per_case_budget = task_budget(&script.group, &script.marker_group);
    let started = current_time();
    let group_deadline = started + group_budget;
    let mut attempted = 0usize;
    let mut passed = 0usize;
    let mut truncated = false;

    println!("#### OS COMP TEST GROUP START {} ####", script.marker_group);
    println!(
        "initproc ltp: walking {} regular cases in lexical order from {}/ltp/testcases/bin",
        cases.len(),
        work_dir.path()
    );
    if let Some(limit) = case_limit {
        println!(
            "initproc ltp: profile case-count budget limits this walk to {} cases",
            limit
        );
    }

    for name in cases.iter() {
        if case_limit.is_some_and(|limit| attempted >= limit) {
            truncated = true;
            println!(
                "initproc watchdog: LTP case-count budget exhausted after {} cases before {}",
                attempted, name
            );
            break;
        }

        let now = current_time();
        if now >= group_deadline {
            truncated = true;
            println!(
                "initproc watchdog: group {} budget exhausted before LTP case {}",
                script.marker_group, name
            );
            break;
        }

        let remaining = group_deadline
            .checked_sub(now)
            .unwrap_or_else(|| Duration::from_secs(0));
        let run_budget = if remaining < per_case_budget {
            remaining
        } else {
            per_case_budget
        };
        let cmd = String::from("ltp/testcases/bin/") + name;
        println!("RUN LTP CASE {}", name);
        if ltp_case_quarantined(name) {
            attempted += 1;
            println!(
                "initproc watchdog: quarantined LTP case {} before known unrecoverable crash storm",
                name
            );
            println!("FAIL LTP CASE {} : {}", name, WATCHDOG_EXIT_CODE);
            kill_all_tasks(REAPER_EXIT_CODE, 16).await;
            clear_ltp_case_scratch(&work_dir);
            continue;
        }
        let result = command_in_dir(
            work_dir.clone(),
            &cmd,
            &script.marker_group,
            run_budget,
            per_case_budget,
        )
        .await;
        let code = command_exit_code(result);
        if code == 0 {
            passed += 1;
        }
        attempted += 1;
        println!("FAIL LTP CASE {} : {}", name, code);
        kill_all_tasks(REAPER_EXIT_CODE, 16).await;
        clear_ltp_case_scratch(&work_dir);
    }

    println!(
        "initproc ltp: attempted {} of {} cases, {} returned 0",
        attempted,
        cases.len(),
        passed
    );
    println!("#### OS COMP TEST GROUP END {} ####", script.marker_group);

    if truncated {
        CommandResult::Truncated
    } else if cases.is_empty() {
        CommandResult::NotFound
    } else {
        CommandResult::Exited(0)
    }
}

fn prepare_group_rootfs(work_dir: &PathBuf, clean_tmp: bool) {
    if clean_tmp {
        clear_dir("/tmp");
    }
    seed_etc_identity_files();
    refresh_busybox_applets(work_dir);
}

fn seed_etc_identity_files() {
    let _ = File::open("/etc", OpenFlags::DIRECTORY | OpenFlags::CREAT);
    write_static_file(
        "/etc/passwd",
        b"root:x:0:0:root:/root:/bin/sh\ndaemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin\nnobody:x:65534:65534:nobody:/tmp:/bin/sh\n",
    );
    write_static_file(
        "/etc/group",
        b"root:x:0:\ndaemon:x:1:\nusers:x:100:\nnogroup:x:65534:\nnobody:x:65534:\n",
    );
}

fn write_static_file(path: &str, content: &[u8]) {
    if let Ok(file) = File::open(path, OpenFlags::CREAT | OpenFlags::RDWR | OpenFlags::TRUNC) {
        let _ = file.truncate(0);
        let _ = file.write(content);
    }
}

fn clear_dir(path: &str) {
    let Ok(dir) = File::open(path, OpenFlags::DIRECTORY) else {
        return;
    };
    let Ok(entries) = dir.read_dir() else {
        return;
    };
    for entry in entries {
        if entry.filename == "." || entry.filename == ".." {
            continue;
        }
        match entry.file_type {
            FileType::Directory => {
                let child = PathBuf::from(path).join(&entry.filename).path();
                let _ = umount(child.clone().into());
                clear_dir(&child);
                let _ = dir.rmdir(&entry.filename);
            }
            _ => {
                let child = PathBuf::from(path).join(&entry.filename).path();
                let _ = umount(child.into());
                let _ = dir.remove(&entry.filename);
            }
        }
    }
}

fn refresh_busybox_applets(work_dir: &PathBuf) {
    let Some(busybox) = find_busybox(work_dir) else {
        return;
    };
    let Ok(bin) = File::open("/bin", OpenFlags::DIRECTORY) else {
        return;
    };
    let Ok(source) = File::open_link(busybox, OpenFlags::RDONLY) else {
        return;
    };
    let source = source.get_bare_file();
    let _ = bin.remove("busybox");
    let _ = bin.link("busybox", source.clone());
    for applet in BUSYBOX_APPLETS {
        let _ = bin.remove(applet);
        let _ = bin.link(applet, source.clone());
    }
}

fn find_busybox(work_dir: &PathBuf) -> Option<String> {
    [
        "busybox",
        "./busybox",
        "../busybox",
        "/busybox",
        "/glibc/busybox",
        "/musl/busybox",
    ]
    .into_iter()
    .map(|candidate| work_dir.join(candidate).path())
    .find(|candidate| File::open_link(candidate.as_str(), OpenFlags::RDONLY).is_ok())
}

fn test_group(script: &str) -> Option<&str> {
    let group = script.strip_suffix("_testcode.sh")?;
    if group.is_empty() || group.starts_with("._") {
        return None;
    }
    Some(group)
}

fn marker_name(line: &str, marker_type: &str) -> Option<String> {
    let prefix = String::from("#### OS COMP TEST GROUP ") + marker_type + " ";
    let start = line.find(prefix.as_str())?;
    let rest = &line[start + prefix.len()..];
    let end = rest.find(" ####")?;
    Some(rest[..end].to_string())
}

fn script_markers(script: &str) -> ScriptMarkers {
    const MAX_SCAN: usize = 65536;
    let Ok(file) = File::open(script, OpenFlags::RDONLY) else {
        return ScriptMarkers::default();
    };
    let size = file.file_size().unwrap_or(0).min(MAX_SCAN);
    if size == 0 {
        return ScriptMarkers::default();
    }

    let mut buffer = vec![0; size];
    match file.readat(0, &mut buffer) {
        Ok(read_size) => {
            let text = String::from_utf8_lossy(&buffer[..read_size]);
            let mut markers = ScriptMarkers::default();
            for line in text.lines() {
                if let Some(group) = marker_name(line, "START") {
                    markers.start = true;
                    if markers.first_group.is_none() {
                        markers.first_group = Some(group);
                    }
                }
                if marker_name(line, "END").is_some() {
                    markers.end = true;
                }
            }
            markers
        }
        Err(_) => ScriptMarkers::default(),
    }
}

fn libc_rank(path: &str) -> usize {
    if path == "/glibc" || path.starts_with("/glibc/") {
        0
    } else if path == "/musl" || path.starts_with("/musl/") {
        1
    } else {
        2
    }
}

fn marker_group(group: &str, dir_path: &str, script_path: &str) -> String {
    let markers = script_markers(script_path);
    if let Some(marker_group) = markers.first_group {
        return marker_group;
    }
    if libc_rank(dir_path) == 0 {
        return String::from(group) + "-glibc";
    }
    if libc_rank(dir_path) == 1 {
        return String::from(group) + "-musl";
    }
    group.to_string()
}

fn scan_test_scripts_in(dir_path: &str) -> Vec<TestScript> {
    let Ok(dir) = File::open(dir_path, OpenFlags::DIRECTORY) else {
        return Vec::new();
    };
    let Ok(entries) = dir.read_dir() else {
        return Vec::new();
    };
    let work_dir = PathBuf::from(dir_path);

    entries
        .into_iter()
        .filter(|entry| entry.file_type == FileType::File)
        .filter_map(|entry| {
            let name = entry.filename;
            let group = test_group(&name)?.to_string();
            let path = work_dir.join(&name).path();
            let markers = script_markers(&path);
            let marker_group = markers
                .first_group
                .clone()
                .unwrap_or_else(|| marker_group(&group, dir_path, &path));
            Some(TestScript {
                path,
                group,
                marker_group,
                markers,
                work_dir: work_dir.clone(),
            })
        })
        .collect()
}

fn scan_test_scripts() -> Vec<TestScript> {
    let mut scripts = scan_test_scripts_in("/");
    let Ok(root) = File::open("/", OpenFlags::DIRECTORY) else {
        return scripts;
    };
    let Ok(entries) = root.read_dir() else {
        return scripts;
    };

    for entry in entries {
        if entry.file_type != FileType::Directory
            || entry.filename == "."
            || entry.filename == ".."
            || entry.filename.starts_with("._")
        {
            continue;
        }
        let dir_path = PathBuf::empty().join(&entry.filename).path();
        scripts.extend(scan_test_scripts_in(&dir_path));
    }

    scripts
}

fn group_rank(group: &str) -> usize {
    GROUP_ORDER
        .iter()
        .position(|candidate| *candidate == group)
        .unwrap_or(GROUP_ORDER.len())
}

fn group_budget(group: &str, marker_group: &str) -> Duration {
    let secs = match marker_group {
        "basic-glibc" | "basic-musl" => 180,
        "busybox-glibc" | "busybox-musl" => 120,
        "lua-glibc" | "lua-musl" => 60,
        "libctest-musl" => 600,
        "libcbench-glibc" => 60,
        "libcbench-musl" => 600,
        "iozone-glibc" => 180,
        "iozone-musl" => 240,
        "lmbench-glibc" => 540,
        "lmbench-musl" => 540,
        "cyclictest-glibc" => 90,
        "cyclictest-musl" => 90,
        "iperf-glibc" => 240,
        "iperf-musl" => 180,
        "netperf-glibc" => 60,
        "netperf-musl" => 240,
        "ltp-glibc" | "ltp-musl" => global_deadline_secs(),
        _ => match group {
            "basic" => 180,
            "busybox" => 120,
            "lua" => 60,
            "libctest" => 600,
            "libcbench" => 600,
            "iozone" => 240,
            "lmbench" => 540,
            "cyclictest" => 180,
            "iperf" => 240,
            "netperf" => 240,
            "ltp" => global_deadline_secs(),
            _ => 60,
        },
    };
    Duration::from_secs(secs)
}

fn task_budget(group: &str, marker_group: &str) -> Duration {
    let secs = match group {
        "ltp" => 15,
        "libctest" => 20,
        "busybox" => 60,
        "lua" => 10,
        "iperf" | "netperf" => 30,
        "cyclictest" if marker_group == "cyclictest-musl" => 15,
        "cyclictest" => 600,
        "libcbench" | "iozone" | "lmbench" => 600,
        "basic" => 20,
        _ => {
            if marker_group.starts_with("busybox-") || marker_group.starts_with("lua-") {
                10
            } else {
                30
            }
        }
    };
    Duration::from_secs(secs)
}

fn skip_script(script: &TestScript) -> bool {
    SKIPPED_MARKER_GROUPS
        .iter()
        .any(|skipped| *skipped == script.marker_group || *skipped == script.group)
}

fn sort_test_scripts(scripts: &mut [TestScript]) {
    scripts.sort_by(|left, right| {
        group_rank(&left.group)
            .cmp(&group_rank(&right.group))
            .then(libc_rank(&left.path).cmp(&libc_rank(&right.path)))
            .then(left.path.cmp(&right.path))
    });
}

async fn run_test_script(script: &TestScript) {
    if skip_script(script) {
        println!(
            "initproc watchdog: group {} skipped by profile-A",
            script.marker_group
        );
        return;
    }

    if script.group == "ltp" {
        let started = current_time();
        let result = run_ltp_cases(script).await;
        let mut status = "not-found";
        let mut exit_code = None;
        match result {
            CommandResult::Exited(code) => {
                status = "exited";
                exit_code = Some(code);
                info!("test script {} exited with {}", script.path, code);
            }
            CommandResult::TimedOut => {
                status = "timed-out";
                println!(
                    "initproc watchdog: group {} exceeded {}s; killed remaining user tasks",
                    script.marker_group,
                    group_budget(&script.group, &script.marker_group).as_secs()
                );
            }
            CommandResult::Truncated => {
                status = "truncated";
                println!(
                    "initproc watchdog: group {} truncated by LTP profile/budget",
                    script.marker_group
                );
            }
            CommandResult::NotFound => {
                println!("test script {} could not be run", script.path);
            }
        }
        let elapsed = current_time()
            .checked_sub(started)
            .unwrap_or_else(|| Duration::from_secs(0));
        if let Some(code) = exit_code {
            println!(
                "initproc watchdog: group {} finished in {}s ({}, code {})",
                script.marker_group,
                elapsed.as_secs(),
                status,
                code
            );
        } else {
            println!(
                "initproc watchdog: group {} finished in {}s ({})",
                script.marker_group,
                elapsed.as_secs(),
                status
            );
        }
        kill_all_tasks(REAPER_EXIT_CODE, 16).await;
        return;
    }

    if !script.markers.start {
        println!("#### OS COMP TEST GROUP START {} ####", script.marker_group);
    }

    let started = current_time();
    let result = script_command(script).await;
    let mut repaired_end = false;
    let mut status = "not-found";
    let mut exit_code = None;
    match result {
        CommandResult::Exited(code) => {
            status = "exited";
            exit_code = Some(code);
            info!("test script {} exited with {}", script.path, code);
        }
        CommandResult::TimedOut => {
            status = "timed-out";
            println!(
                "initproc watchdog: group {} exceeded {}s; killed remaining user tasks",
                script.marker_group,
                group_budget(&script.group, &script.marker_group).as_secs()
            );
            println!("#### OS COMP TEST GROUP END {} ####", script.marker_group);
            repaired_end = true;
        }
        CommandResult::Truncated => {
            status = "truncated";
            println!(
                "initproc watchdog: group {} truncated by profile/budget",
                script.marker_group
            );
            println!("#### OS COMP TEST GROUP END {} ####", script.marker_group);
            repaired_end = true;
        }
        CommandResult::NotFound => {
            println!("test script {} could not be run", script.path);
        }
    }

    if !script.markers.end && !repaired_end {
        println!("#### OS COMP TEST GROUP END {} ####", script.marker_group);
    }
    let elapsed = current_time()
        .checked_sub(started)
        .unwrap_or_else(|| Duration::from_secs(0));
    if let Some(code) = exit_code {
        println!(
            "initproc watchdog: group {} finished in {}s ({}, code {})",
            script.marker_group,
            elapsed.as_secs(),
            status,
            code
        );
    } else {
        println!(
            "initproc watchdog: group {} finished in {}s ({})",
            script.marker_group,
            elapsed.as_secs(),
            status
        );
    }

    kill_all_tasks(REAPER_EXIT_CODE, 16).await;
}

pub async fn initproc() {
    println!("start kernel tasks");
    // command("./runtest.exe -w entry-dynamic.exe argv").await;
    // command("./entry-dynamic.exe argv").await;
    // command("busybox echo run time-test").await;
    // command("time-test").await;

    // command("busybox sh basic/run-all.sh").await;

    // command("busybox echo run netperf_testcode.sh").await;
    // command("busybox sh netperf_testcode.sh").await;

    // command("busybox echo run busybox_testcode.sh").await;
    // command("busybox sh busybox_testcode.sh").await;

    // command("busybox echo run libctest_testcode.sh").await;
    // command("busybox sh libctest_testcode.sh").await;
    // command("runtest.exe -w entry-static.exe utime").await;
    // command("busybox ln -s /busybox /bin/cat").await;
    // command("./bin/cat libctest_testcode.sh").await;
    // command("busybox ls -l /bin").await;
    // command("busybox ln -s /busybox /bin/ln").await;
    // command("busybox ln -s /busybox /bin/wget").await;
    // command("busybox ln -s /busybox /bin/xz").await;
    // command("busybox ls -l /bin").await;
    // command("busybox sh init.sh").await;
    // command("busybox ls -l /bin").await;

    let mut scripts = scan_test_scripts();
    sort_test_scripts(&mut scripts);
    if scripts.is_empty() {
        println!("No *_testcode.sh found on root filesystem");
    } else {
        let global_deadline_secs = global_deadline_secs();
        arm_global_deadline_after(Duration::from_secs(global_deadline_secs));
        let global_deadline = current_time() + Duration::from_secs(global_deadline_secs);
        for script in scripts.iter() {
            if current_time() >= global_deadline {
                println!(
                    "initproc watchdog: global deadline {}s reached; skipping remaining test groups",
                    global_deadline_secs
                );
                break;
            }
            run_test_script(script).await;
        }
        disarm_global_deadline();
    }

    kill_all_tasks(REAPER_EXIT_CODE, 16).await;
    shutdown();
}
