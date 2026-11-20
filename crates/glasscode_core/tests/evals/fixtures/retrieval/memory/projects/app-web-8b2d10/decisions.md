# decisions

## Colours

- Use the tokens in `src/theme/tokens.ts`. <!-- id=r070 status=confirmed kind=fact source=words:W070 -->
- No raw hex colours in components. <!-- id=r071 status=confirmed kind=fact source=words:W071 -->

## State

- Server state goes through TanStack Query. <!-- id=r072 status=confirmed kind=fact source=words:W072 -->
- No global store. <!-- id=r073 status=confirmed kind=fact source=words:W073 -->

## Retry with backoff

- The front end never retries POST requests. <!-- id=r074 status=confirmed kind=fact source=words:W074 -->
- GET requests retry once. <!-- id=r075 status=confirmed kind=fact source=words:W075 -->

## Routing

- Routes are file-based under `src/routes`. <!-- id=r076 status=confirmed kind=fact source=words:W076 -->
- No hash routing. <!-- id=r077 status=confirmed kind=fact source=words:W077 -->

## Size budget

- `maxBundleKb` is 250. <!-- id=r078 status=confirmed kind=fact source=words:W078 -->
