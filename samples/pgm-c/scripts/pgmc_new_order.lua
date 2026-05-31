-- PGM-C New-Order Transaction
-- Creates a new order with 5-15 order lines

local warehouse_id = rand_int(min_id, max_id)
local district_id = rand_int(1, 10)
local customer_id = rand_int(1, const.customers_per_district)
local ol_cnt = rand_int(5, 15)
local ol_all_local = 1
local rbk = rand_int(1, 100)

-- Pre-generate order lines
local order_lines = {}
local item_ids = {}

for ol_number = 1, ol_cnt do
    local ol_supply_w_id = warehouse_id
    local ol_quantity = rand_int(1, 10)

    -- Generate unique item id
    local ol_i_id
    repeat
        ol_i_id = rand_int(1, const.items)
    until not item_ids[ol_i_id]
    item_ids[ol_i_id] = true

    -- 1% rollback via invalid item (on last line)
    if rbk == 1 and ol_number == ol_cnt then
        ol_i_id = 999999
    end

    -- Remote warehouse 1% of the time
    if (max_id - min_id) > 0 and rand_int(1, 100) == 1 then
        ol_all_local = 0
        while ol_supply_w_id == warehouse_id do
            ol_supply_w_id = rand_int(min_id, max_id)
        end
    end

    table.insert(order_lines, {ol_number, ol_supply_w_id, ol_quantity, ol_i_id})
end

begin()

-- Get warehouse tax
query("SELECT w_tax FROM warehouse WHERE w_id = $1", warehouse_id)

-- Get next order id and district tax
local dist_rows = query(
    "UPDATE district SET d_next_o_id = d_next_o_id + 1 WHERE d_w_id = $1 AND d_id = $2 RETURNING d_tax, d_next_o_id",
    warehouse_id, district_id
)
local o_id = dist_rows[1]["d_next_o_id"] - 1

-- Get customer discount, last name, credit
query(
    "SELECT c_discount, c_last, c_credit FROM customer WHERE c_w_id = $1 AND c_d_id = $2 AND c_id = $3",
    warehouse_id, district_id, customer_id
)

-- Insert order
exec(
    "INSERT INTO orders (o_id, o_d_id, o_w_id, o_c_id, o_entry_d, o_ol_cnt, o_all_local) VALUES ($1, $2, $3, $4, NOW(), $5, $6)",
    o_id, district_id, warehouse_id, customer_id, ol_cnt, ol_all_local
)

-- Insert new_order
exec(
    "INSERT INTO new_order (no_o_id, no_d_id, no_w_id) VALUES ($1, $2, $3)",
    o_id, district_id, warehouse_id
)

-- Build stock query for this district
local stock_query = string.format(
    "SELECT s_quantity, s_dist_%02d AS s_dist, s_data FROM stock WHERE s_i_id = $1 AND s_w_id = $2",
    district_id
)

for _, ol in ipairs(order_lines) do
    local ol_number = ol[1]
    local ol_supply_w_id = ol[2]
    local ol_quantity = ol[3]
    local ol_i_id = ol[4]

    -- Get item info
    local item_rows = query("SELECT i_price, i_name, i_data FROM item WHERE i_id = $1", ol_i_id)
    if #item_rows == 0 then
        rollback()
        error("New-order rollback: item not found")
    end

    local i_price = item_rows[1]["i_price"]
    local ol_amount = i_price * ol_quantity

    -- Get stock info
    local stock_rows = query(stock_query, ol_i_id, ol_supply_w_id)
    local s_quantity = stock_rows[1]["s_quantity"]
    local s_dist = stock_rows[1]["s_dist"]

    -- Adjust stock quantity
    if (s_quantity - ol_quantity) > 10 then
        s_quantity = s_quantity - ol_quantity
    else
        s_quantity = s_quantity - ol_quantity + 91
    end

    local s_remote_cnt_inc = 0.0
    if ol_supply_w_id ~= warehouse_id then
        s_remote_cnt_inc = 1.0
    end

    -- Update stock
    exec(
        "UPDATE stock SET s_quantity = $3, s_ytd = s_ytd + $4::NUMERIC, s_order_cnt = s_order_cnt + 1, s_remote_cnt = s_remote_cnt + $5 WHERE s_i_id = $1 AND s_w_id = $2",
        ol_i_id, ol_supply_w_id, s_quantity, ol_quantity, s_remote_cnt_inc
    )

    -- Insert order line
    exec(
        "INSERT INTO order_line (ol_o_id, ol_d_id, ol_w_id, ol_number, ol_i_id, ol_supply_w_id, ol_quantity, ol_amount, ol_dist_info) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        o_id, district_id, warehouse_id, ol_number, ol_i_id, ol_supply_w_id, ol_quantity, ol_amount, s_dist
    )
end

commit()
