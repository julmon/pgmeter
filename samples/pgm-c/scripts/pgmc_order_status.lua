-- PGM-C Order-Status Transaction
-- Queries the status of a customer's last order

local warehouse_id = rand_int(min_id, max_id)
local district_id = rand_int(1, 10)
local y = rand_int(1, 100)
local c_id = rand_int(1, const.customers_per_district)

-- gen_last() comes from scripts/pgmc_common.lua (scripts.modules)

begin()

-- Customer lookup: 60% by last name, 40% by ID
if y <= 60 then
    local c_last = gen_last(rand_int(1, 999))
    local c_rows = query(
        "SELECT c_id FROM customer WHERE c_w_id = $1 AND c_d_id = $2 AND c_last = $3 ORDER BY c_first ASC",
        warehouse_id, district_id, c_last
    )
    if #c_rows == 0 then
        rollback()
        error("Order-Status rollback: customer not found")
    end
    local n = #c_rows
    c_id = c_rows[math.floor(n / 2) + 1]["c_id"]
end

-- Get customer info
query(
    "SELECT c_balance, c_first, c_middle, c_last FROM customer WHERE c_w_id = $1 AND c_d_id = $2 AND c_id = $3",
    warehouse_id, district_id, c_id
)

-- Get last order
local o_rows = query(
    "SELECT o_id FROM orders WHERE o_w_id = $1 AND o_d_id = $2 AND o_c_id = $3 ORDER BY o_entry_d DESC LIMIT 1",
    warehouse_id, district_id, c_id
)

if #o_rows > 0 then
    local o_id = o_rows[1]["o_id"]

    -- Get order lines
    query(
        "SELECT ol_i_id, ol_supply_w_id, ol_quantity, ol_amount, ol_delivery_d FROM order_line WHERE ol_w_id = $1 AND ol_d_id = $2 AND ol_o_id = $3",
        warehouse_id, district_id, o_id
    )
end

commit()
