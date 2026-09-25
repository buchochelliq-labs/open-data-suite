select
    id as order_id,
    user_id as customer_id,
    cast(order_date as date) as order_date,
    status
from {{ ref('raw_orders') }}
