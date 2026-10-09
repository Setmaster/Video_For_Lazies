//! Ownership of a media command includes its descendants and its exit waiter.
//! Pipe readers can keep draining while cancellation kills the process tree;
//! no blocking wait holds the handle used for termination.

use std::cell::RefCell;
use std::io;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Clone)]
pub(crate) struct JobContext {
    pub cancel: Arc<AtomicBool>,
    pub child: Arc<Mutex<Option<ProcessControl>>>,
}

thread_local! {
    static JOB_CONTEXT: RefCell<Option<JobContext>> = const { RefCell::new(None) };
}

/// The encode worker is synchronous. Its nested capability/probe commands
/// inherit this context without making standalone UI inspections cancellable
/// by an unrelated export. A scope always restores its previous context.
pub(crate) struct JobScope {
    previous: Option<JobContext>,
    // Restoring a thread-local value on a different thread would be incorrect.
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl JobScope {
    pub fn enter(context: JobContext) -> Self {
        Self {
            previous: JOB_CONTEXT.replace(Some(context)),
            _thread_bound: std::marker::PhantomData,
        }
    }
}

impl Drop for JobScope {
    fn drop(&mut self) {
        JOB_CONTEXT.set(self.previous.take());
    }
}

pub(crate) fn check_cancelled() -> io::Result<()> {
    if JOB_CONTEXT.with_borrow(|context| {
        context
            .as_ref()
            .is_some_and(|context| context.cancel.load(Ordering::Relaxed))
    }) {
        Err(io::Error::new(io::ErrorKind::Interrupted, "Canceled."))
    } else {
        Ok(())
    }
}

struct ProcessState {
    tree: Mutex<Option<platform::ProcessTree>>,
    exited: Mutex<bool>,
    exit_event: Condvar,
    #[cfg(unix)]
    pipe_exit: std::os::unix::net::UnixStream,
    #[cfg(unix)]
    pipe_exit_signal: std::os::unix::net::UnixStream,
}

#[derive(Clone)]
pub(crate) struct ProcessControl(Arc<ProcessState>);

impl ProcessControl {
    pub fn kill(&self) -> io::Result<()> {
        let guard = self
            .0
            .tree
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        guard.as_ref().map_or(Ok(()), platform::ProcessTree::kill)
    }

    fn retire(&self) {
        // On Linux the unreaped leader still reserves the process-group ID.
        // On Windows the job handle is an identity, not a reusable PID.
        let mut guard = self
            .0
            .tree
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(tree) = guard.take() {
            let _ = tree.kill();
        }
    }

    fn mark_exited(&self) {
        *self
            .0
            .exited
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = true;
        #[cfg(unix)]
        let _ = self.0.pipe_exit_signal.shutdown(std::net::Shutdown::Write);
        self.0.exit_event.notify_all();
    }

    pub fn wait_for_exit(&self, timeout: Duration) -> bool {
        let exited = self
            .0
            .exited
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (exited, _) = self
            .0
            .exit_event
            .wait_timeout_while(exited, timeout, |exited| !*exited)
            .unwrap_or_else(|error| error.into_inner());
        *exited
    }

    fn wait_until_exited(&self) {
        let exited = self
            .0
            .exited
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        drop(
            self.0
                .exit_event
                .wait_while(exited, |exited| !*exited)
                .unwrap_or_else(|error| error.into_inner()),
        );
    }
}

#[derive(Default)]
struct Registry {
    stopping: bool,
    processes: Vec<Weak<ProcessState>>,
}

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

fn stop_registry(registry: &Mutex<Registry>) {
    let controls: Vec<_> = {
        let mut registry = registry.lock().unwrap_or_else(|error| error.into_inner());
        registry.stopping = true;
        registry
            .processes
            .drain(..)
            .filter_map(|process| process.upgrade())
            .map(ProcessControl)
            .collect()
    };
    for control in &controls {
        let _ = control.kill();
    }
    for control in controls {
        control.wait_until_exited();
    }
}

/// Called only after the real window is destroyed, never at CloseRequested.
/// This also owns standalone probes once command dispatch becomes asynchronous.
pub(crate) fn shutdown_all() {
    stop_registry(REGISTRY.get_or_init(Mutex::default));
}

pub(crate) struct ManagedChild {
    pub stdout: Option<PipeReader<ChildStdout>>,
    pub stderr: Option<PipeReader<ChildStderr>>,
    pub control: ProcessControl,
    exit: mpsc::Receiver<io::Result<ExitStatus>>,
    waiter: Option<JoinHandle<()>>,
    job: Option<JobContext>,
}

impl ManagedChild {
    pub fn spawn(command: Command) -> io::Result<Self> {
        let job = JOB_CONTEXT.with_borrow(Clone::clone);
        Self::spawn_in(command, job, REGISTRY.get_or_init(Mutex::default))
    }

    pub fn spawn_for_job(command: Command, job: JobContext) -> io::Result<Self> {
        Self::spawn_in(command, Some(job), REGISTRY.get_or_init(Mutex::default))
    }

    fn spawn_in(
        mut command: Command,
        job: Option<JobContext>,
        registry: &Mutex<Registry>,
    ) -> io::Result<Self> {
        if job
            .as_ref()
            .is_some_and(|job| job.cancel.load(Ordering::Relaxed))
        {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "Canceled."));
        }
        // Keep spawn and registration atomic relative to application shutdown.
        let mut registry = registry.lock().unwrap_or_else(|error| error.into_inner());
        if registry.stopping {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "The application is closing.",
            ));
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        let (pipe_exit, pipe_exit_signal) = std::os::unix::net::UnixStream::pair()?;
        let (mut child, tree) = platform::spawn(command)?;
        let control = ProcessControl(Arc::new(ProcessState {
            tree: Mutex::new(Some(tree)),
            exited: Mutex::new(false),
            exit_event: Condvar::new(),
            #[cfg(unix)]
            pipe_exit,
            #[cfg(unix)]
            pipe_exit_signal,
        }));
        registry
            .processes
            .retain(|process| process.strong_count() > 0);
        registry.processes.push(Arc::downgrade(&control.0));
        if let Some(job) = &job {
            let mut slot = job.child.lock().unwrap_or_else(|error| error.into_inner());
            *slot = Some(control.clone());
            if job.cancel.load(Ordering::Relaxed) {
                let _ = control.kill();
            }
        }
        let stdout = child
            .stdout
            .take()
            .map(|pipe| PipeReader::new(pipe, control.clone()));
        let stderr = child
            .stderr
            .take()
            .map(|pipe| PipeReader::new(pipe, control.clone()));
        let wait_control = control.clone();
        let (sender, exit) = mpsc::channel();
        // Keep a recovery owner until the waiter thread actually starts. A
        // failed thread allocation must not leave a child or registry entry
        // which shutdown would wait for forever.
        let owner = Arc::new(Mutex::new(Some(child)));
        let waiter_owner = owner.clone();
        let waiter = std::thread::Builder::new()
            .name("media-process-exit".to_string())
            .spawn(move || {
                let mut child = waiter_owner.lock().unwrap().take().unwrap();
                let result = platform::wait(&mut child, &wait_control);
                wait_control.mark_exited();
                let _ = sender.send(result);
            });
        let waiter = match waiter {
            Ok(waiter) => waiter,
            Err(error) => {
                let _ = control.kill();
                if let Some(mut child) = owner.lock().unwrap().take() {
                    let _ = platform::wait(&mut child, &control);
                }
                control.mark_exited();
                if let Some(job) = &job {
                    *job.child.lock().unwrap_or_else(|error| error.into_inner()) = None;
                }
                return Err(error);
            }
        };
        drop(registry);
        Ok(Self {
            stdout,
            stderr,
            control,
            exit,
            waiter: Some(waiter),
            job,
        })
    }

    pub fn wait_timeout(&self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        match self.exit.recv_timeout(timeout) {
            Ok(result) => result.map(Some),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(io::Error::other("Process exit waiter stopped."))
            }
        }
    }

    pub fn wait(&self) -> io::Result<ExitStatus> {
        self.exit
            .recv()
            .map_err(|_| io::Error::other("Process exit waiter stopped."))?
    }
}

pub(crate) struct PipeReader<R> {
    pipe: R,
    #[cfg(unix)]
    control: ProcessControl,
    #[cfg(unix)]
    drain_deadline: Option<std::time::Instant>,
}

impl<R> PipeReader<R> {
    fn new(pipe: R, _control: ProcessControl) -> Self {
        Self {
            pipe,
            #[cfg(unix)]
            control: _control,
            #[cfg(unix)]
            drain_deadline: None,
        }
    }
}

#[cfg(windows)]
impl<R: io::Read> io::Read for PipeReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        // The Windows job does not permit descendant breakaway. Terminating
        // it closes every inherited writer, including on normal parent exit.
        self.pipe.read(output)
    }
}

#[cfg(unix)]
impl<R: io::Read + std::os::fd::AsRawFd> io::Read for PipeReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        use std::os::fd::AsRawFd;
        if output.is_empty() {
            return Ok(0);
        }
        let fd = self.pipe.as_raw_fd();
        if self.drain_deadline.is_none() {
            let (ready, exited) = platform::wait_pipe(fd, self.control.0.pipe_exit.as_raw_fd())?;
            if !exited && ready {
                return self.pipe.read(output);
            }
            self.drain_deadline = Some(std::time::Instant::now() + Duration::from_millis(100));
        }
        // A hung-up pipe cannot gain more bytes, so drain its entire buffered
        // tail regardless of consumer scheduling. An open writer gets only a
        // bounded grace after process exit, even if it writes continuously.
        platform::wait_pipe_after_exit(fd, self.drain_deadline.unwrap())?;
        self.pipe.read(output)
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        let _ = self.control.kill();
        if let Some(waiter) = self.waiter.take() {
            let _ = waiter.join();
        }
        if let Some(job) = &self.job {
            let mut slot = job.child.lock().unwrap_or_else(|error| error.into_inner());
            if slot
                .as_ref()
                .is_some_and(|control| Arc::ptr_eq(&control.0, &self.control.0))
            {
                *slot = None;
            }
        }
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::unix::process::CommandExt;

    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
        #[cfg(target_os = "linux")]
        fn waitid(id_type: i32, id: u32, info: *mut std::ffi::c_void, options: i32) -> i32;
        fn poll(fds: *mut PollFd, count: usize, timeout_ms: i32) -> i32;
    }

    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        returned: i16,
    }

    fn poll_retry(fds: &mut [PollFd], timeout_ms: i32) -> io::Result<i32> {
        let deadline = (timeout_ms >= 0)
            .then(|| std::time::Instant::now() + Duration::from_millis(timeout_ms as u64));
        loop {
            let timeout_ms = deadline.map_or(-1, |deadline| {
                deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .as_millis()
                    .min(i32::MAX as u128) as i32
            });
            let result = unsafe { poll(fds.as_mut_ptr(), fds.len(), timeout_ms) };
            if result >= 0 {
                return Ok(result);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    pub fn wait_pipe(fd: i32, exit_fd: i32) -> io::Result<(bool, bool)> {
        let mut fds = [
            PollFd {
                fd,
                events: 1,
                returned: 0,
            },
            PollFd {
                fd: exit_fd,
                events: 1,
                returned: 0,
            },
        ];
        poll_retry(&mut fds, -1)?;
        Ok((fds[0].returned != 0, fds[1].returned != 0))
    }

    pub fn wait_pipe_after_exit(fd: i32, deadline: std::time::Instant) -> io::Result<()> {
        let mut fds = [PollFd {
            fd,
            events: 1,
            returned: 0,
        }];
        // Ordinary killed descendants close promptly. This grace is only used
        // after leader exit and an empty pipe; readable data never polls on a
        // timer. A Unix wrapper can deliberately escape its process group, so
        // keep that writer from escaping the command's IO completion bound.
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        poll_retry(&mut fds, timeout_ms)?;
        if fds[0].returned & 0x10 != 0
            || (fds[0].returned != 0 && std::time::Instant::now() < deadline)
        {
            Ok(())
        } else {
            Err(io::Error::other(
                "Process output pipe remained open after the command exited.",
            ))
        }
    }

    pub struct ProcessTree(u32);

    impl ProcessTree {
        pub fn kill(&self) -> io::Result<()> {
            // The isolated group contains only this command and its descendants.
            if unsafe { kill(-(self.0 as i32), 9) } == 0 {
                Ok(())
            } else {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(3) {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }

    pub fn spawn(mut command: Command) -> io::Result<(Child, ProcessTree)> {
        command.process_group(0);
        let child = command.spawn()?;
        let tree = ProcessTree(child.id());
        Ok((child, tree))
    }

    pub fn wait(child: &mut Child, control: &ProcessControl) -> io::Result<ExitStatus> {
        #[cfg(target_os = "linux")]
        {
            // WNOWAIT leaves the leader unreaped while descendants are killed,
            // so a recycled process-group ID can never target another command.
            // Linux siginfo_t is 128 bytes; u64 storage supplies its alignment.
            let mut info = [0u64; 16];
            loop {
                let result =
                    unsafe { waitid(1, child.id(), info.as_mut_ptr().cast(), 4 | 0x0100_0000) };
                if result == 0 {
                    break;
                }
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    control.retire();
                    let _ = child.wait();
                    return Err(error);
                }
            }
            control.retire();
            child.wait()
        }
        #[cfg(not(target_os = "linux"))]
        {
            let result = child.wait();
            control.retire();
            result
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::time::Instant;

    pub(crate) fn fixture_command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        let module = module_path!().split_once("::").unwrap().1;
        command
            .args([
                "--exact",
                &format!("{module}::process_fixture"),
                "--nocapture",
            ])
            .env("VFL_PROCESS_TEST_FIXTURE", mode);
        command
    }

    /// The fixture deliberately leaves containment. A disposable lease makes
    /// it exit on cleanup without ever signaling a potentially recycled PID.
    #[cfg(unix)]
    pub(crate) struct EscapedFixture(std::path::PathBuf);

    #[cfg(unix)]
    impl EscapedFixture {
        pub(crate) fn new(stream: &str, wait: bool) -> (Self, Command) {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "vfl-escaped-fixture-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("lease"), []).unwrap();
            let mut command = fixture_command("escaped");
            command
                .env("VFL_PROCESS_ESCAPED_ROOT", &root)
                .env("VFL_PROCESS_ESCAPED_STREAM", stream)
                .env("VFL_PROCESS_ESCAPED_WAIT", if wait { "1" } else { "0" });
            (Self(root), command)
        }
    }

    #[cfg(unix)]
    impl Drop for EscapedFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0.join("lease"));
            let deadline = Instant::now() + Duration::from_secs(2);
            while self.0.join("ready").exists()
                && !self.0.join("finished").exists()
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = std::fs::remove_file(self.0.join("ready"));
            let _ = std::fs::remove_file(self.0.join("finished"));
            let _ = std::fs::remove_dir(&self.0);
        }
    }

    #[test]
    fn process_fixture() {
        let Ok(mode) = std::env::var("VFL_PROCESS_TEST_FIXTURE") else {
            return;
        };
        match mode.as_str() {
            "sleep" => {
                println!("fixture-ready");
                std::io::stdout().flush().unwrap();
                std::thread::sleep(Duration::from_secs(30));
            }
            "stdout-eof" => {
                std::io::stdout().flush().unwrap();
                #[cfg(unix)]
                unsafe {
                    unsafe extern "C" {
                        fn close(fd: i32) -> i32;
                    }
                    assert_eq!(close(1), 0);
                }
                #[cfg(windows)]
                unsafe {
                    #[link(name = "kernel32")]
                    unsafe extern "system" {
                        fn GetStdHandle(kind: u32) -> *mut std::ffi::c_void;
                        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
                    }
                    assert_ne!(CloseHandle(GetStdHandle(-11i32 as u32)), 0);
                }
                std::thread::sleep(Duration::from_secs(30));
            }
            "descendant" => {
                // Deliberately drop the handle while the descendant retains
                // both inherited pipes. The supervisor must kill it on exit.
                #[allow(clippy::zombie_processes)] // The parent fixture intentionally abandons it.
                let _descendant = fixture_command("sleep").spawn().unwrap();
                println!("descendant-started");
            }
            #[cfg(unix)]
            "escaped" => {
                use std::os::unix::process::CommandExt;
                let mut command = fixture_command("escaped-writer");
                if std::env::var("VFL_PROCESS_ESCAPED_STREAM").unwrap() == "stdout" {
                    command.stderr(Stdio::null());
                } else {
                    command.stdout(Stdio::null());
                }
                unsafe {
                    command.pre_exec(|| {
                        unsafe extern "C" {
                            fn setsid() -> i32;
                        }
                        if setsid() < 0 {
                            Err(io::Error::last_os_error())
                        } else {
                            Ok(())
                        }
                    });
                }
                #[allow(clippy::zombie_processes)]
                // EscapedFixture owns the detached writer's lease.
                let _descendant = command.spawn().unwrap();
                let root =
                    std::path::PathBuf::from(std::env::var_os("VFL_PROCESS_ESCAPED_ROOT").unwrap());
                let deadline = Instant::now() + Duration::from_secs(5);
                while !root.join("ready").exists() {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
                if std::env::var("VFL_PROCESS_ESCAPED_WAIT").unwrap() == "1" {
                    std::thread::sleep(Duration::from_secs(30));
                }
            }
            #[cfg(unix)]
            "escaped-writer" => {
                let root =
                    std::path::PathBuf::from(std::env::var_os("VFL_PROCESS_ESCAPED_ROOT").unwrap());
                if root.join("lease").exists() {
                    let _ = std::fs::write(root.join("ready"), []);
                    let deadline = Instant::now() + Duration::from_secs(30);
                    while root.join("lease").exists() && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    let _ = std::fs::write(root.join("finished"), []);
                }
                // Avoid the test harness writing to a pipe already closed by
                // its owner after the bounded reader returned an error.
                std::process::exit(0);
            }
            "stdin" => {
                let mut input = Vec::new();
                std::io::stdin().read_to_end(&mut input).unwrap();
                assert!(input.is_empty());
                println!("stdin-was-eof");
            }
            "large" => {
                let data = vec![0xd7; 256 * 1024];
                std::io::stdout().write_all(&data).unwrap();
                std::io::stderr().write_all(&data).unwrap();
            }
            "tail" => {
                std::io::stdout().write_all(&[0xd7; 1024]).unwrap();
                std::io::stderr().write_all(&[0xd7; 1024]).unwrap();
            }
            "success" => println!("fixture-success"),
            _ => panic!("Unknown process fixture"),
        }
    }

    fn collect(mut child: ManagedChild) -> (ExitStatus, Vec<u8>, Vec<u8>) {
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        std::thread::scope(|scope| {
            let stdout = scope.spawn(move || {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).unwrap();
                bytes
            });
            let stderr = scope.spawn(move || {
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).unwrap();
                bytes
            });
            let status = child.wait_timeout(Duration::from_secs(5)).unwrap();
            if status.is_none() {
                child.control.kill().unwrap();
            }
            let stdout = stdout.join().unwrap();
            let stderr = stderr.join().unwrap();
            (
                status.expect("fixture did not exit within 5 seconds"),
                stdout,
                stderr,
            )
        })
    }

    #[test]
    fn process_drains_both_pipes_and_disables_inherited_stdin() {
        let (status, stdout, stderr) =
            collect(ManagedChild::spawn(fixture_command("large")).unwrap());
        assert!(status.success());
        assert_eq!(
            stdout.iter().filter(|&&byte| byte == 0xd7).count(),
            256 * 1024
        );
        assert_eq!(stderr, vec![0xd7; 256 * 1024]);
        let (status, stdout, _) = collect(ManagedChild::spawn(fixture_command("stdin")).unwrap());
        assert!(status.success());
        assert!(String::from_utf8_lossy(&stdout).contains("stdin-was-eof"));
    }

    #[test]
    fn process_exit_terminates_descendants_holding_both_pipes() {
        let started = Instant::now();
        let (status, stdout, _) =
            collect(ManagedChild::spawn(fixture_command("descendant")).unwrap());
        assert!(status.success());
        assert!(String::from_utf8_lossy(&stdout).contains("descendant-started"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn process_preserves_output_tail_when_reader_is_delayed_after_exit() {
        let mut child = ManagedChild::spawn(fixture_command("tail")).unwrap();
        assert!(
            child
                .wait_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap()
                .success()
        );
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let mut first = [0u8; 1];
        stdout.read_exact(&mut first).unwrap();
        let mut bytes = Vec::from(first);
        std::thread::sleep(Duration::from_millis(150));
        stdout.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.iter().filter(|&&byte| byte == 0xd7).count(), 1024);
        bytes.clear();
        stderr.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, vec![0xd7; 1024]);
    }

    #[test]
    #[cfg(unix)]
    fn process_shutdown_unblocks_readers_even_when_writer_escaped_the_group() {
        let registry = Mutex::new(Registry::default());
        let (_fixture, command) = EscapedFixture::new("stdout", false);
        let mut child = ManagedChild::spawn_in(command, None, &registry).unwrap();
        assert!(
            child
                .wait_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap()
                .success()
        );
        stop_registry(&registry);
        let started = Instant::now();
        let error = child
            .stdout
            .take()
            .unwrap()
            .read_to_end(&mut Vec::new())
            .unwrap_err();
        assert!(error.to_string().contains("pipe remained open"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn process_can_be_cancelled_after_stdout_eof_before_exit() {
        let mut child = ManagedChild::spawn(fixture_command("stdout-eof")).unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (send, receive) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.read_to_end(&mut bytes);
            let _ = send.send(result);
        });
        let eof = receive.recv_timeout(Duration::from_secs(5));
        let was_running = child
            .wait_timeout(Duration::from_millis(10))
            .unwrap()
            .is_none();
        child.control.kill().unwrap();
        let status = child.wait_timeout(Duration::from_secs(5)).unwrap();
        reader.join().unwrap();
        assert!(eof.unwrap().is_ok());
        assert!(was_running);
        assert!(!status.unwrap().success());
    }

    #[test]
    fn process_job_scope_tracks_preflight_and_blocks_later_work_after_cancel() {
        let job = JobContext {
            cancel: Arc::new(AtomicBool::new(false)),
            child: Arc::new(Mutex::new(None)),
        };
        {
            let _scope = JobScope::enter(job.clone());
            let child = ManagedChild::spawn(fixture_command("sleep")).unwrap();
            let control = job.child.lock().unwrap().as_ref().unwrap().clone();
            job.cancel.store(true, Ordering::Relaxed);
            control.kill().unwrap();
            assert!(
                !child
                    .wait_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap()
                    .success()
            );
            drop(child);
            assert!(job.child.lock().unwrap().is_none());
            assert_eq!(
                check_cancelled().unwrap_err().kind(),
                io::ErrorKind::Interrupted
            );
            assert!(
                matches!(ManagedChild::spawn(fixture_command("success")), Err(error) if error.kind() == io::ErrorKind::Interrupted)
            );
        }
        assert!(check_cancelled().is_ok());
    }

    #[test]
    fn process_publication_honors_cancellation_after_spawn_begins() {
        let registry = Arc::new(Mutex::new(Registry::default()));
        let job = JobContext {
            cancel: Arc::new(AtomicBool::new(false)),
            child: Arc::new(Mutex::new(None)),
        };
        // Hold publication so cancellation deterministically falls after the
        // initial flag check but before the handle is placed into the slot.
        let slot = job.child.lock().unwrap();
        let spawning_registry = registry.clone();
        let spawning_job = job.clone();
        let worker = std::thread::spawn(move || {
            ManagedChild::spawn_in(
                fixture_command("sleep"),
                Some(spawning_job),
                &spawning_registry,
            )
        });
        let started = Instant::now();
        loop {
            if matches!(
                registry.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ) {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(1));
        }
        job.cancel.store(true, Ordering::Relaxed);
        drop(slot);
        let child = worker.join().unwrap().unwrap();
        assert!(
            !child
                .wait_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap()
                .success()
        );
        drop(child);
        assert!(job.child.lock().unwrap().is_none());
    }

    #[test]
    fn process_shutdown_kills_running_work_and_rejects_queued_work() {
        let registry = Mutex::new(Registry::default());
        let child = ManagedChild::spawn_in(fixture_command("sleep"), None, &registry).unwrap();
        stop_registry(&registry);
        assert!(
            !child
                .wait_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap()
                .success()
        );
        assert!(
            matches!(ManagedChild::spawn_in(fixture_command("success"), None, &registry), Err(error) if error.kind() == io::ErrorKind::Interrupted)
        );
    }

    #[cfg(unix)]
    #[test]
    fn short_process_completion_does_not_wait_for_a_polling_tick() {
        let mut samples = Vec::new();
        for _ in 0..15 {
            let mut command = Command::new("sh");
            command.args(["-c", "exit 0"]);
            let started = Instant::now();
            assert!(collect(ManagedChild::spawn(command).unwrap()).0.success());
            samples.push(started.elapsed());
        }
        samples.sort();
        // Median tolerates scheduler noise but detects the old 50 ms floor.
        assert!(
            samples[7] < Duration::from_millis(40),
            "median {:?}",
            samples[7]
        );
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::ffi::c_void;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::os::windows::process::CommandExt;

    type Handle = *mut c_void;

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        process_user_time: i64,
        job_user_time: i64,
        flags: u32,
        minimum_working_set: usize,
        maximum_working_set: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io_counters: [u64; 6],
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory: usize,
        peak_job_memory: usize,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ThreadEntry {
        size: u32,
        usage: u32,
        thread_id: u32,
        process_id: u32,
        base_priority: i32,
        delta_priority: i32,
        flags: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: i32, info: *const c_void, size: u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn TerminateJobObject(job: Handle, code: u32) -> i32;
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> Handle;
        fn Thread32First(snapshot: Handle, entry: *mut ThreadEntry) -> i32;
        fn Thread32Next(snapshot: Handle, entry: *mut ThreadEntry) -> i32;
        fn OpenThread(access: u32, inherit: i32, id: u32) -> Handle;
        fn ResumeThread(thread: Handle) -> u32;
    }

    pub struct ProcessTree(OwnedHandle);

    impl ProcessTree {
        pub fn kill(&self) -> io::Result<()> {
            if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }

    fn resume_primary_thread(process_id: u32) -> io::Result<()> {
        let raw = unsafe { CreateToolhelp32Snapshot(4, 0) };
        if raw == -1isize as Handle {
            return Err(io::Error::last_os_error());
        }
        let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut entry = ThreadEntry {
            size: std::mem::size_of::<ThreadEntry>() as u32,
            ..Default::default()
        };
        let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
        while found != 0 {
            if entry.process_id == process_id {
                let raw = unsafe { OpenThread(2, 0, entry.thread_id) };
                if raw.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let thread = unsafe { OwnedHandle::from_raw_handle(raw) };
                if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                return Ok(());
            }
            found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
        }
        Err(io::Error::other(
            "Could not find the suspended media process thread.",
        ))
    }

    pub fn spawn(mut command: Command) -> io::Result<(Child, ProcessTree)> {
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut limits = ExtendedLimits::default();
        limits.basic.flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                9,
                (&limits as *const ExtendedLimits).cast(),
                std::mem::size_of::<ExtendedLimits>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // Assignment precedes any child execution, including descendants of an
        // external wrapper. Nested jobs are supported on our Windows baseline.
        command.creation_flags(0x0800_0000 | 4); // CREATE_NO_WINDOW | CREATE_SUSPENDED
        let mut child = command.spawn()?;
        if unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) } == 0 {
            let error = io::Error::last_os_error();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let tree = ProcessTree(job);
        if let Err(error) = resume_primary_thread(child.id()) {
            let _ = tree.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok((child, tree))
    }

    pub fn wait(child: &mut Child, control: &ProcessControl) -> io::Result<ExitStatus> {
        let result = child.wait();
        control.retire();
        result
    }
}
