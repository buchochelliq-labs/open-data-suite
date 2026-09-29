-- Reads `a`. Set B_FAILS=1 to make it fail, and unset it to fix it: an environment
-- variable, not a var, because `dbt retry` replays the original run's vars.
{% if env_var('B_FAILS', '0') == '1' %}
select * from this_table_does_not_exist
{% else %}
select built_in as a_built_in from {{ ref('a') }}
{% endif %}
