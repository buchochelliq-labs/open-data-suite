
-- Deliberate `select *` over a ref: column resolution needs the upstream schema.
select * from "jaffle_ods"."main"."customers"