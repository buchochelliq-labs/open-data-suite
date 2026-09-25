select
    id as payment_id,
    order_id,
    payment_method,
    (amount_cents / 100.0) as amount
from "jaffle_ods"."main"."raw_payments"