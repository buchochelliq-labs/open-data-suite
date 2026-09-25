select
    order_id,
    customer_id,
    amount,
    row_number() over (partition by customer_id order by order_date) as order_seq,
    sum(amount) over (partition by customer_id order by order_date) as running_amount
from "jaffle_ods"."main"."orders"
where status in ('completed', 'shipped')