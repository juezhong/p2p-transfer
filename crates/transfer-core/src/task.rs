//! Deterministic transfer task lifecycle and progress accounting.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskPhase {
    Pending,
    Running,
    Verifying,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskError {
    InvalidTransition,
    InvalidProgress,
    AlreadyTerminal,
}

#[derive(Debug)]
pub struct Task {
    pub id: TaskId,
    phase: TaskPhase,
    total_bytes: u64,
    written_bytes: u64,
}

impl Task {
    pub fn new(id: TaskId, total_bytes: u64) -> Self {
        Self {
            id,
            phase: TaskPhase::Pending,
            total_bytes,
            written_bytes: 0,
        }
    }

    pub fn phase(&self) -> TaskPhase {
        self.phase
    }
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }
    pub fn written_bytes(&self) -> u64 {
        self.written_bytes
    }

    pub fn start(&mut self) -> Result<(), TaskError> {
        if self.phase != TaskPhase::Pending {
            return Err(TaskError::InvalidTransition);
        }
        self.phase = TaskPhase::Running;
        Ok(())
    }

    /// Represents contiguous bytes written by the receiver, not fsync durability.
    pub fn acknowledge_written(&mut self, next_offset: u64) -> Result<(), TaskError> {
        if self.phase != TaskPhase::Running {
            return Err(TaskError::InvalidTransition);
        }
        if next_offset < self.written_bytes || next_offset > self.total_bytes {
            return Err(TaskError::InvalidProgress);
        }
        self.written_bytes = next_offset;
        Ok(())
    }

    pub fn begin_verification(&mut self) -> Result<(), TaskError> {
        if self.phase != TaskPhase::Running || self.written_bytes != self.total_bytes {
            return Err(TaskError::InvalidTransition);
        }
        self.phase = TaskPhase::Verifying;
        Ok(())
    }

    /// Call only after successful content hash verification and file commit.
    pub fn complete(&mut self) -> Result<(), TaskError> {
        if self.phase != TaskPhase::Verifying {
            return Err(TaskError::InvalidTransition);
        }
        self.phase = TaskPhase::Completed;
        Ok(())
    }

    pub fn cancel(&mut self) -> Result<(), TaskError> {
        if matches!(
            self.phase,
            TaskPhase::Completed | TaskPhase::Cancelled | TaskPhase::Failed
        ) {
            return Err(TaskError::AlreadyTerminal);
        }
        self.phase = TaskPhase::Cancelled;
        Ok(())
    }

    pub fn fail(&mut self) -> Result<(), TaskError> {
        if matches!(
            self.phase,
            TaskPhase::Completed | TaskPhase::Cancelled | TaskPhase::Failed
        ) {
            return Err(TaskError::AlreadyTerminal);
        }
        self.phase = TaskPhase::Failed;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cannot_complete_before_verification() {
        let mut task = Task::new(TaskId(1), 20);
        assert!(task.complete().is_err());
        task.start().unwrap();
        task.acknowledge_written(20).unwrap();
        assert!(task.complete().is_err());
        task.begin_verification().unwrap();
        task.complete().unwrap();
        assert_eq!(task.phase(), TaskPhase::Completed);
    }

    #[test]
    fn rejects_ack_regression_and_overflow() {
        let mut task = Task::new(TaskId(2), 10);
        task.start().unwrap();
        task.acknowledge_written(5).unwrap();
        assert_eq!(task.acknowledge_written(4), Err(TaskError::InvalidProgress));
        assert_eq!(
            task.acknowledge_written(11),
            Err(TaskError::InvalidProgress)
        );
        assert_eq!(task.written_bytes(), 5);
    }

    #[test]
    fn cancellation_is_terminal() {
        let mut task = Task::new(TaskId(3), 10);
        task.start().unwrap();
        task.cancel().unwrap();
        assert_eq!(task.cancel(), Err(TaskError::AlreadyTerminal));
        assert!(task.begin_verification().is_err());
    }

    #[test]
    fn zero_byte_file_still_requires_verification() {
        let mut task = Task::new(TaskId(4), 0);
        task.start().unwrap();
        task.begin_verification().unwrap();
        task.complete().unwrap();
        assert_eq!(task.phase(), TaskPhase::Completed);
    }
}
