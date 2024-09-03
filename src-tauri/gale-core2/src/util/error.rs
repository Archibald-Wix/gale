use anyhow::Context;
use std::path::Path;
pub trait IoResultExt<T> {
    fn fs_context(self, op: &str, path: &Path) -> anyhow::Result<T>;
}

impl<T, E> IoResultExt<T> for std::result::Result<T, E>
where
    E: std::error::Error + Send + Sync + 'static,
{
    fn fs_context(self, op: &str, path: &Path) -> anyhow::Result<T> {
        self.with_context(|| format!("error while {} (at {})", op, path.display()))
    }
}
