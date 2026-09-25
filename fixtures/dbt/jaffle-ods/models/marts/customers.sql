with order_stats as (
    select
        customer_id,
        min(order_date) as first_order,
        max(order_date) as most_recent_order,
        count(order_id) as number_of_orders,
        sum(amount) as lifetime_value
    from {{ ref('orders') }}
    where status <> 'returned'
    group by customer_id
)

select
    c.customer_id,
    c.first_name,
    c.last_name,
    c.first_name || ' ' || c.last_name as full_name,
    s.first_order,
    s.most_recent_order,
    coalesce(s.number_of_orders, 0) as number_of_orders,
    coalesce(s.lifetime_value, 0) as lifetime_value,
    case when s.lifetime_value >= 20 then 'high' else 'standard' end as value_tier
from {{ ref('stg_customers') }} as c
left join order_stats as s using (customer_id)
