{% macro cents_to_dollars(column) -%}
    ({{ column }} / 100.0)
{%- endmacro %}
