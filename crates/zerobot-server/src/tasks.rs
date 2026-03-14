use std::sync::{Arc, Mutex};

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

impl From<TaskScheduler> for Arc<TaskScheduler> {
    fn from(value: TaskScheduler) -> Self {
        Arc::new(value)
    }
}
