# configmerge

Builds one configuration from layers, later layers winning:

1. defaults (in code),
2. the config file (`key = value` lines, `[section]` headers, `#` comments),
3. environment variables with the `APP__` prefix: `APP__DB__PORT=5432` sets `db.port`.

Merging is deep: a layer that sets `db.port` leaves `db.host` from earlier layers alone.
Values are typed: integers, booleans (`true`/`false`, any case), lists (`a,b,c`, empty items
dropped) and strings.
