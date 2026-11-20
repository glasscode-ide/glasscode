# decisions

## Keep the v1 API

- Keep the v1 API until every client has moved to v2. <!-- id=r032 status=confirmed kind=fact source=words:W032 -->
- No removal before 2027. <!-- id=r033 status=confirmed kind=fact source=words:W033 -->

## Retry with backoff

- HTTP calls retry three times. <!-- id=r034 status=confirmed kind=fact source=words:W034 -->
- `retryBackoffMs` is 250 and doubles on each try. <!-- id=r035 status=confirmed kind=fact source=words:W035 -->

## Session tokens

- Session tokens live 12 hours and refresh tokens 30 days. <!-- id=r036 status=confirmed kind=fact source=words:W036 -->
- They are kept in an httpOnly cookie. <!-- id=r037 status=confirmed kind=fact source=words:W037 -->

## Feature flags

- New features ship behind `FEATURE_FLAG_*` environment variables. <!-- id=r038 status=confirmed kind=fact source=words:W038 -->
- Remove a flag within two releases. <!-- id=r039 status=confirmed kind=fact source=words:W039 -->

## Logging

- Use the structured logger in `src/log.ts`. <!-- id=r040 status=confirmed kind=fact source=words:W040 -->
- Never log request bodies. <!-- id=r041 status=confirmed kind=fact source=words:W041 -->

## Time zones

- Store times in UTC. <!-- id=r042 status=confirmed kind=fact source=words:W042 -->
- Convert to local time only in the front end. <!-- id=r043 status=confirmed kind=fact source=words:W043 -->

## Dependencies

- No new runtime dependency without a short note in the pull request on why. <!-- id=r044 status=confirmed kind=fact source=words:W044 -->
