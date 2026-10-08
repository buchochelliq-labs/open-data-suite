select * from (values
    (1, 1, date '2026-01-02', 12.50),
    (2, 1, date '2026-01-05', 30.00),
    (3, 2, date '2026-01-05', 8.75)
) as t (order_id, customer_id, ordered_at, amount)
