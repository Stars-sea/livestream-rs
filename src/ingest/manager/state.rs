/// Coarse lifecycle state for manager-level stream orchestration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerStreamState {
    Created,
    Starting,
    Running,
    Stopping,
    Stopped,
}
