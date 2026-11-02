-- An incremental model (#435): each run adds the payments it hasn't seen yet, by id.
-- It is off the stg_orders path the docs demo edits.
{{ config(materialized='incremental', unique_key='payment_id') }}

select
    payment_id,
    order_id,
    payment_method,
    amount
from {{ ref('stg_payments') }}
{% if is_incremental() %}
where payment_id > (select coalesce(max(payment_id), 0) from {{ this }})
{% endif %}
