{{ config(materialized='view') }}
-- Deliberate `select *` over a ref: column resolution needs the upstream schema.
select * from {{ ref('customers') }}
