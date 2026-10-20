pub const GREETING: &str = "hello";

pub fn greet(name: &str) -> String {
    format!("{GREETING}, {name}")
}
