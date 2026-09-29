# A Python model (#171): SQL analysis can't see inside it, so ODS treats it as opaque
# (anything it reads may affect every column) unless observed lineage says otherwise.
def model(dbt, session):
    dbt.config(materialized="table")
    customers = dbt.ref("customers")
    return customers.project(
        "customer_id, "
        "case when lifetime_value >= 100 then 'vip' else 'regular' end as segment"
    )
