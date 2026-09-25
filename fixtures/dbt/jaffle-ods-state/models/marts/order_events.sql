{{ config(state={'execute_hooks_on_any_reuse': true}) }}
select order_id, order_date as event_date, 'ordered' as event_type
from {{ ref('stg_orders') }}
union all
select order_id, cast(null as date) as event_date, 'paid' as event_type
from {{ ref('stg_payments') }}
