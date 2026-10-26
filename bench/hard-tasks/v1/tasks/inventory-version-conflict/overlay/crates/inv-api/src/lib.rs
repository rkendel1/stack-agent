//! Request handling: validation, the store, and status codes.

use inv_model::Sku;
use inv_store::{Store, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Available { sku: String },
    SetOnHand { sku: String, on_hand: u32, version: u64 },
    Reserve { sku: String, quantity: u32, ttl_ms: u64, version: u64 },
    Release { reservation: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

fn ok(body: String) -> Response {
    Response { status: 200, body }
}

fn bad(body: &str) -> Response {
    Response { status: 400, body: body.to_string() }
}

fn error(e: StoreError) -> Response {
    match e {
        StoreError::UnknownSku(s) => Response { status: 404, body: format!("unknown sku {s}") },
        StoreError::UnknownReservation(id) => Response { status: 404, body: format!("unknown reservation {id}") },
        StoreError::Conflict { expected, actual, .. } => {
            Response { status: 500, body: format!("version conflict: expected {expected}, now {actual}") }
        }
        StoreError::Insufficient { requested, available, .. } => {
            Response { status: 409, body: format!("insufficient stock: requested {requested}, available {available}") }
        }
    }
}

/// Handles one request at `now_ms`. Expired reservations are swept first.
pub fn handle(store: &mut Store, request: Request, now_ms: u64) -> Response {
    store.sweep(now_ms);
    let sku_of = |s: &str| Sku::parse(s);
    match request {
        Request::Available { sku } => match sku_of(&sku) {
            None => bad("invalid sku"),
            Some(sku) => store.available(&sku).map(|n| ok(n.to_string())).unwrap_or_else(error),
        },
        Request::SetOnHand { sku, on_hand, version } => match sku_of(&sku) {
            None => bad("invalid sku"),
            Some(sku) => store.set_on_hand(&sku, on_hand, version).map(|v| ok(format!("version {v}"))).unwrap_or_else(error),
        },
        Request::Reserve { sku, quantity, ttl_ms, version } => {
            if quantity == 0 {
                return bad("quantity must be positive");
            }
            if ttl_ms == 0 || ttl_ms > 86_400_000 {
                return bad("ttl must be between 1 ms and one day");
            }
            match sku_of(&sku) {
                None => bad("invalid sku"),
                Some(sku) => store
                    .reserve(&sku, quantity, now_ms + ttl_ms, version)
                    .map(|(id, v)| ok(format!("reservation {id} version {v}")))
                    .unwrap_or_else(error),
            }
        }
        Request::Release { reservation } => store.release(reservation).map(|_| ok("released".into())).unwrap_or_else(error),
    }
}
