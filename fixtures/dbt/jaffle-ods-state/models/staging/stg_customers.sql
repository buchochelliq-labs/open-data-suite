select
    id as customer_id,
    first_name,
    last_name,
    lower(email) as email,
    cast(signup_date as date) as signup_date
from {{ ref('raw_customers') }}
