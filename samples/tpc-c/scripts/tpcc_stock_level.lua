-- TPC-C Stock-Level Transaction
-- Checks stock levels for recently ordered items

local warehouse_id = rand_int(min_id, max_id)
local district_id = rand_int(1, 10)
local threshold = rand_int(10, 20)

begin_tx()

-- Get next order id for this district
local next_o_id = query_one(
    "SELECT d_next_o_id FROM district WHERE d_w_id = $1 AND d_id = $2",
    warehouse_id, district_id
)

-- Count distinct items below threshold in recent orders
query(
    "SELECT COUNT(DISTINCT s_i_id) FROM order_line JOIN stock ON s_i_id = ol_i_id AND s_w_id = $1 WHERE ol_w_id = $1 AND ol_d_id = $2 AND ol_o_id BETWEEN $3 - 20 AND $3 - 1 AND s_quantity < $4",
    warehouse_id, district_id, next_o_id, threshold
)

commit()
