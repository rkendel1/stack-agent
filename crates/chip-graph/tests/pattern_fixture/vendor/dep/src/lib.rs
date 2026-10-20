// A vendored copy. Its structure is not this repository's implementation.
pub trait Handler {
    fn handle(&self);
}
pub struct V1;
pub struct V2;
pub struct V3;
impl Handler for V1 {
    fn handle(&self) {}
}
impl Handler for V2 {
    fn handle(&self) {}
}
impl Handler for V3 {
    fn handle(&self) {}
}
