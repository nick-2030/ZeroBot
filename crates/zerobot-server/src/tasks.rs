use std::sync::Mutex;

#[derive(Default)]
pub struct TaskScheduler {
    running: Mutex<bool>,
}

impl TaskScheduler {
    pub fn start(&self) {
        let mut guard = self.running.lock().unwrap();
        *guard = true;
    }
}
