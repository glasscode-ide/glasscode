# decisions

## Rate limits

- Clients get 600 requests a minute. <!-- id=r107 status=confirmed kind=fact source=words:W107 -->
- `rateLimitPerMinute` lives in `config/api.ts`. <!-- id=r108 status=confirmed kind=fact source=words:W108 -->

## Pagination

- List endpoints use cursor pagination with a `next_cursor` field. <!-- id=r109 status=confirmed kind=fact source=words:W109 -->
- No page numbers. <!-- id=r110 status=confirmed kind=fact source=words:W110 -->

## Errors

- Errors follow RFC 9457 problem details. <!-- id=r111 status=confirmed kind=fact source=words:W111 -->
