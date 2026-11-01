-- PGM-C Payment Transaction
-- Processes a customer payment

local warehouse_id = rand_int(min_id, max_id)
local district_id = rand_int(1, 10)
local x = rand_int(1, 100)
local y = rand_int(1, 100)

local c_d_id, c_w_id
if x <= 85 then
    c_d_id = district_id
    c_w_id = warehouse_id
else
    c_d_id = rand_int(1, 10)
    if (max_id - min_id) > 0 then
        c_w_id = warehouse_id
        while c_w_id == warehouse_id do
            c_w_id = rand_int(min_id, max_id)
        end
    else
        c_w_id = warehouse_id
    end
end

local c_id = nurand(1023, C_ID, 1, const.customers_per_district)
-- TPC-C 2.5.1.3: H_AMOUNT is a cent amount in [1.00 .. 5000.00]. Kept as a
-- decimal string so the same value reaches w_ytd, d_ytd, c_balance,
-- c_ytd_payment and h_amount, and sum(h_amount) stays equal to w_ytd.
local cents = rand_int(100, 500000)
local h_amount = string.format("%d.%02d", cents // 100, cents % 100)

-- gen_last() comes from scripts/pgmc_common.lua (scripts.modules)

local c_last = ""
if y <= 60 then
    c_last = gen_last(nurand(255, C_LAST_RUN, 0, 999))
end

begin()

-- Update warehouse year-to-date
local w_rows = query(
    "UPDATE warehouse SET w_ytd = w_ytd + $1::NUMERIC WHERE w_id = $2 RETURNING w_name",
    h_amount, warehouse_id
)
local w_name = w_rows[1]["w_name"]

-- Update district year-to-date
local d_rows = query(
    "UPDATE district SET d_ytd = d_ytd + $1::NUMERIC WHERE d_w_id = $2 AND d_id = $3 RETURNING d_name",
    h_amount, warehouse_id, district_id
)
local d_name = d_rows[1]["d_name"]

-- Customer lookup
if y <= 60 then
    -- By last name
    local c_rows = query(
        "SELECT c_id FROM customer WHERE c_w_id = $1 AND c_d_id = $2 AND c_last = $3 ORDER BY c_first ASC",
        c_w_id, c_d_id, c_last
    )
    if #c_rows == 0 then
        rollback()
        error("Payment rollback: customer not found by last name")
    end
    -- TPC-C 2.5.2.2: the customer at position n/2 rounded up.
    local n = #c_rows
    c_id = c_rows[math.ceil(n / 2)]["c_id"]
end

-- Get customer credit status
local cust_rows = query(
    "SELECT c_credit FROM customer WHERE c_w_id = $1 AND c_d_id = $2 AND c_id = $3",
    c_w_id, c_d_id, c_id
)
local c_credit = cust_rows[1]["c_credit"]

-- Update customer (TPC-C 2.5.2.2): the payment comes off the balance and is
-- added to the year-to-date payments, the payment count goes up by one, and a
-- bad-credit customer gets the payment recorded at the front of c_data.
if c_credit ~= nil and string.find(c_credit, "BC") then
    local pre_c_data = string.format("%d %d %d %d %d %s", c_id, c_d_id, c_w_id, district_id, warehouse_id, h_amount)
    exec(
        "UPDATE customer SET c_balance = c_balance - $1::NUMERIC, c_ytd_payment = c_ytd_payment + $1::NUMERIC, c_payment_cnt = c_payment_cnt + 1, c_data = substring($5||' '||c_data, 1, 500) WHERE c_id = $2 AND c_d_id = $3 AND c_w_id = $4",
        h_amount, c_id, c_d_id, c_w_id, pre_c_data
    )
else
    exec(
        "UPDATE customer SET c_balance = c_balance - $1::NUMERIC, c_ytd_payment = c_ytd_payment + $1::NUMERIC, c_payment_cnt = c_payment_cnt + 1 WHERE c_id = $2 AND c_d_id = $3 AND c_w_id = $4",
        h_amount, c_id, c_d_id, c_w_id
    )
end

-- Insert history
local h_data = string.sub((w_name or "") .. "    " .. (d_name or ""), 1, 24)
exec(
    "INSERT INTO history (h_c_id, h_c_d_id, h_c_w_id, h_d_id, h_w_id, h_date, h_amount, h_data) VALUES ($1, $2, $3, $4, $5, NOW(), $6, $7)",
    c_id, c_d_id, c_w_id, district_id, warehouse_id, h_amount, h_data
)

commit()
