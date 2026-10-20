pub fn create_user(x: u32) -> Result<(), String> {
    validate_input(x)?;
    write(x);
    Ok(())
}

pub fn create_order(x: u32) -> Result<(), String> {
    let checked = validate_input(x)?;
    write(checked);
    Ok(())
}

pub fn create_item(x: u32) -> Result<(), String> {
    validate_input(x)?;
    log(x);
    write(x);
    Ok(())
}

// The call in the argument is evaluated first, so validation precedes the write here too.
pub fn create_nested(x: u32) -> Result<(), String> {
    write(validate_input(x)?);
    Ok(())
}

// Divergent order: the write comes first.
pub fn legacy_import(x: u32) -> Result<(), String> {
    write(x);
    validate_input(x)?;
    Ok(())
}

// Both calls are here, but the validation is only under a condition: no order is established, and
// "validated before written" must not be claimed.
pub fn maybe_validated(x: u32, strict: bool) {
    if strict {
        let _ = validate_input(x);
    }
    write(x);
}

// The validation is in a closure, which may never run.
pub fn deferred_validation(x: u32) {
    let check = || validate_input(x);
    write(x);
    let _ = check;
}

pub fn validate_input(x: u32) -> Result<u32, String> {
    Ok(x)
}

pub fn write(_x: u32) {}

fn log(_x: u32) {}
