{% set methods = ['credit_card', 'coupon', 'bank_transfer', 'gift_card'] %}

with payments as (
    select
        order_id,
        {% for m in methods -%}
        sum(case when payment_method = '{{ m }}' then amount else 0 end) as {{ m }}_amount,
        {% endfor -%}
        sum(amount) as total_amount
    from {{ ref('stg_payments') }}
    group by order_id
)

select
    o.order_id,
    o.customer_id,
    o.order_date,
    o.status,
    {% for m in methods -%}
    p.{{ m }}_amount,
    {% endfor -%}
    coalesce(p.total_amount, 0) as amount
from {{ ref('stg_orders') }} as o
left join payments as p on o.order_id = p.order_id
