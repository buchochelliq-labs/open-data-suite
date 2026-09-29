-- A SQL model reading a Python model: its lineage stops at the opaque model's columns.
select
    segment,
    count(*) as customers
from "jaffle_ods"."main"."customer_segments"
group by segment