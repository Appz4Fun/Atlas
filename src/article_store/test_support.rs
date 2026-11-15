//! Small shared ownership/readiness helpers for actual subprocess controls.
use std::{
    fs,
    io::{self, Read},
    path::Path,
    process::Child,
    thread,
    time::{Duration, Instant},
};
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
pub(super) struct OwnedChild(pub(super) Child);
impl std::ops::Deref for OwnedChild {
    type Target = Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        // Child caches a reaped exit status, so normal explicit wait/try_wait paths
        // never signal a reused PID. Every live/unresolved child is killed and reaped.
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
pub(super) fn wait_exit(child: &mut Child) -> std::process::ExitStatus {
    let until = deadline();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            panic!("bounded subprocess did not exit");
        }
        thread::sleep(Duration::from_millis(10));
    }
}
pub(super) fn marker_ready(path: &Path, expected: &[u8]) -> bool {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return false,
        Err(error) => panic!("cannot open child boundary marker: {error}"),
    };
    // Read only the expected signal plus one byte, so empty/partial/wrong/extra
    // contents cannot be mistaken for a complete semantic boundary.
    let mut observed = Vec::with_capacity(expected.len() + 1);
    file.take(expected.len() as u64 + 1).read_to_end(&mut observed).expect("read bounded child boundary marker");
    observed == expected
}
pub(super) fn wait_marker(child: &mut Child, path: &Path, expected: &[u8]) {
    let until = deadline();
    loop {
        if marker_ready(path, expected) {
            return;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("child exited before semantic boundary marker: {status}");
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            panic!("bounded subprocess did not reach semantic boundary");
        }
        thread::sleep(Duration::from_millis(10));
    }
}
