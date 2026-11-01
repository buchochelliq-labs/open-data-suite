select customer_id, min(ordered_at) as first_ordered_at
from {{ ref('orders') }}
group by customer_id
