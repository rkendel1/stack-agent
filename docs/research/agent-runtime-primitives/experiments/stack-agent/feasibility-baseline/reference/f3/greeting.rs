pub const GREETING: &str = "welcome";

pub fn greet(name: &str) -> String {
    format!("{GREETING}, {name}")
}
