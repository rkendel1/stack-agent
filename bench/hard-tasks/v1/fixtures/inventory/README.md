# inventory

Stock levels with reservations, for a small shop.

* `inv-model`: SKUs, quantities, items.
* `inv-store`: the stock store. Every item has a version; updates name the version they expect
  (optimistic concurrency). Reservations hold stock for a while and expire.
* `inv-api`: request handling: validates requests, calls the store, maps errors to statuses.

## Reservations

A reservation holds `quantity` of a SKU until `expires_at_ms`. Expired reservations are released
by the expiry sweep, which lives in `inv-api/src/expire.rs` and runs before every request.
A reservation expires at its expiry time: at `now == expires_at_ms` it no longer holds stock.

## Statuses

`200` ok, `400` invalid request, `404` unknown SKU, `409` version conflict or insufficient stock.
