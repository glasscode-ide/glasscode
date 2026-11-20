# decisions

## Invoices are exported nightly

- The invoice export runs at 01:00 UTC from `src/billing/invoiceExporter.ts`. <!-- id=r087 status=confirmed kind=fact source=words:W087 -->

## Money as integers

- Amounts are stored as integer cents. <!-- id=r088 status=confirmed kind=fact source=words:W088 -->
- Never use floats for money. <!-- id=r089 status=confirmed kind=fact source=words:W089 -->

## Currency

- Only EUR and USD are supported. <!-- id=r090 status=confirmed kind=fact source=words:W090 -->
- Adding a currency needs Sam's sign-off. <!-- id=r091 status=confirmed kind=fact source=words:W091 -->

## Webhooks

- Payment webhooks are checked with the signing secret. <!-- id=r092 status=confirmed kind=fact source=words:W092 -->
- The provider retries them for 3 days. <!-- id=r093 status=confirmed kind=fact source=words:W093 -->

## Idempotency

- Every payment request carries an `Idempotency-Key` header built from the invoice id. <!-- id=r094 status=confirmed kind=fact source=words:W094 -->
