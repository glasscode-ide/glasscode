# performance

## Slow dashboard query

- The dashboard query needs the `idx_orders_customer_created` index. <!-- id=r053 status=confirmed kind=fact source=words:W053 -->
- Without it the query's p95 is 4 s. <!-- id=r054 status=confirmed kind=fact source=words:W054 -->

## Caching

- Customer summaries are cached for 60 seconds in Redis under `summary:<id>`. <!-- id=r055 status=confirmed kind=fact source=words:W055 -->
