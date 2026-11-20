# migrations

## Writing a migration

- Migrations are forward-only. <!-- id=r095 status=confirmed kind=fact source=words:W095 -->
- Add a column, backfill it, then switch the reads. <!-- id=r096 status=confirmed kind=fact source=words:W096 -->

## Locks

- Avoid ALTER TABLE on `invoices` during business hours: it locks writes. <!-- id=r097 status=confirmed kind=fact source=words:W097 -->
