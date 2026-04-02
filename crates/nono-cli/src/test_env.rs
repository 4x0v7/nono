/// Process-global lock for tests that mutate environment variables.
///
/// Rust unit tests run in parallel within the same process, so concurrent
/// `env::set_var` / `env::remove_var` calls race against each other.
/// All env-mutating tests must acquire this lock before touching env vars.
///
/// See <https://github.com/always-further/nono/issues/567> for the plan to
/// eliminate env var mutation from tests entirely.
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
