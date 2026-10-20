pub trait Handler {
    fn validate(&self);
    fn handle(&self);
}

pub struct Alpha;
pub struct Beta;
pub struct Gamma;
pub struct Delta;
pub struct Old;

impl Handler for Alpha {
    fn validate(&self) {}
    fn handle(&self) {}
}

impl Handler for Beta {
    fn handle(&self) {}
    fn validate(&self) {}
}

impl Handler for Gamma {
    fn validate(&self) {}
    fn handle(&self) {}
}

// Divergent: no validation of its own.
impl Handler for Delta {
    fn validate(&self) {}
}

#[deprecated]
impl Handler for Old {
    fn handle(&self) {}
    fn validate(&self) {}
    fn migrate(&self) {}
}
