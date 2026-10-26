-- PGM-C Delivery Transaction
-- Processes pending orders for all 10 districts of a warehouse

local warehouse_id = rand_int(min_id, max_id)
local carrier_id = rand_int(1, 10)

begin()

for district_id = 1, 10 do
    -- Find the oldest undelivered order and lock it. SKIP LOCKED makes two
    -- clients delivering the same warehouse/district pick different orders
    -- instead of both processing the same one (the second DELETE would then
    -- affect no row while orders and customer were still updated).
    local rows = query(
        "SELECT no_o_id FROM new_order WHERE no_w_id = $1 AND no_d_id = $2 ORDER BY no_o_id ASC LIMIT 1 FOR UPDATE SKIP LOCKED",
        warehouse_id, district_id
    )

    if #rows > 0 then
        local order_id = rows[1]["no_o_id"]

        -- Delete from new_order
        exec(
            "DELETE FROM new_order WHERE no_o_id = $1 AND no_w_id = $2 AND no_d_id = $3",
            order_id, warehouse_id, district_id
        )

        -- Update orders with carrier_id, get customer_id
        local customer_id = query_one(
            "UPDATE orders SET o_carrier_id = $1 WHERE o_id = $2 AND o_w_id = $3 AND o_d_id = $4 RETURNING o_c_id",
            carrier_id, order_id, warehouse_id, district_id
        )

        -- Update order_line delivery dates
        exec(
            "UPDATE order_line SET ol_delivery_d = current_timestamp WHERE ol_o_id = $1 AND ol_w_id = $2 AND ol_d_id = $3",
            order_id, warehouse_id, district_id
        )

        -- Sum order line amounts
        local total = query_one(
            "SELECT COALESCE(SUM(ol_amount * ol_quantity), 0) FROM order_line WHERE ol_o_id = $1 AND ol_w_id = $2 AND ol_d_id = $3",
            order_id, warehouse_id, district_id
        )

        -- Update customer balance and delivery count
        exec(
            "UPDATE customer SET c_delivery_cnt = c_delivery_cnt + 1, c_balance = c_balance + $1 WHERE c_id = $2 AND c_w_id = $3 AND c_d_id = $4",
            total, customer_id, warehouse_id, district_id
        )
    end
end

commit()
