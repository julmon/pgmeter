-- PGM-H Data Loading Script
-- Populates part, supplier, partsupp, customer, orders, lineitem following
-- the rules of the TPC-H data generator (dbgen's build.c and dists.dss):
-- value lists and ranges, price per part key, supplier bridge, customer
-- mortality (one customer in three never orders), order status from the
-- lines shipped by the "current date" 1995-06-17, and the comment markers
-- that Q13 and Q16 search for.
--
-- Parallel model: `ids` is this job's slice of 1..scalefactor. Each id is one
-- self-contained "unit" worth one SF of data, with surrogate keys offset by
-- (unit-1)*per_unit_count so units never collide and every job loads in
-- parallel. Cross-table references (partsupp->part/supplier, orders->customer,
-- lineitem->orders/partsupp) are kept *within* the same unit, so the foreign
-- keys hold without any cross-unit coordination.

-- Per-unit sizing (one scalefactor's worth)
local PARTS_PER_UNIT = 200000
local SUPPLIERS_PER_UNIT = 10000
local CUSTOMERS_PER_UNIT = 150000
local ORDERS_PER_UNIT = 1500000
local CLERKS_PER_UNIT = 1000
local PARTSUPPS_PER_PART = 4
local BATCH = 10000
local SUPPLIER_BATCH = 5000

-- Reference data
local SEGMENTS = {"AUTOMOBILE", "BUILDING", "FURNITURE", "HOUSEHOLD", "MACHINERY"}
local PRIORITIES = {"1-URGENT", "2-HIGH", "3-MEDIUM", "4-NOT SPECIFIED", "5-LOW"}
local SHIP_MODES = {"REG AIR", "AIR", "RAIL", "SHIP", "TRUCK", "MAIL", "FOB"}
local SHIP_INSTRUCT = {"DELIVER IN PERSON", "COLLECT COD", "NONE", "TAKE BACK RETURN"}
local CONTAINERS = {
    "SM CASE", "SM BOX", "SM PACK", "SM PKG", "SM JAR", "SM BAG", "SM CAN", "SM DRUM",
    "MED CASE", "MED BOX", "MED PACK", "MED PKG", "MED JAR", "MED BAG", "MED CAN", "MED DRUM",
    "LG CASE", "LG BOX", "LG PACK", "LG PKG", "LG JAR", "LG BAG", "LG CAN", "LG DRUM",
    "JUMBO CASE", "JUMBO BOX", "JUMBO PACK", "JUMBO PKG", "JUMBO JAR", "JUMBO BAG", "JUMBO CAN", "JUMBO DRUM",
    "WRAP CASE", "WRAP BOX", "WRAP PACK", "WRAP PKG", "WRAP JAR", "WRAP BAG", "WRAP CAN", "WRAP DRUM",
}
-- The 150 part types of dists.dss: 6 x 5 x 5 words.
local T1 = {"STANDARD", "SMALL", "MEDIUM", "LARGE", "ECONOMY", "PROMO"}
local T2 = {"ANODIZED", "BURNISHED", "PLATED", "POLISHED", "BRUSHED"}
local T3 = {"TIN", "NICKEL", "BRASS", "STEEL", "COPPER"}

-- The 92 colours of dists.dss; a part name is five distinct ones.
local PART_WORDS = {
    "almond", "antique", "aquamarine", "azure", "beige", "bisque", "black", "blanched",
    "blue", "blush", "brown", "burlywood", "burnished", "chartreuse", "chiffon", "chocolate",
    "coral", "cornflower", "cornsilk", "cream", "cyan", "dark", "deep", "dim", "dodger",
    "drab", "firebrick", "floral", "forest", "frosted", "gainsboro", "ghost", "goldenrod",
    "green", "grey", "honeydew", "hot", "indian", "ivory", "khaki", "lace", "lavender",
    "lawn", "lemon", "light", "lime", "linen", "magenta", "maroon", "medium", "metallic",
    "midnight", "mint", "misty", "moccasin", "navajo", "navy", "olive", "orange", "orchid",
    "pale", "papaya", "peach", "peru", "pink", "plum", "powder", "puff", "purple", "red",
    "rose", "rosy", "royal", "saddle", "salmon", "sandy", "seashell", "sienna", "sky",
    "slate", "smoke", "snow", "spring", "steel", "tan", "thistle", "tomato", "turquoise",
    "violet", "wheat", "white", "yellow",
}

-- The word pairs Q13 looks for in order comments (dists.dss Q13a/Q13b).
-- dbgen's text grammar puts them in a few percent of the comments; here
-- one comment in four gets one of the 16 pairs, so each pair matches about
-- 1.5% of the orders.
local Q13_ADJ = {"special", "pending", "unusual", "express"}
local Q13_NOUN = {"packages", "requests", "accounts", "deposits"}

local function pick(t) return t[rand_int(1, #t)] end

local function gen_part_name()
    local words, n = {}, 0
    while n < 5 do
        local w = pick(PART_WORDS)
        local dup = false
        for k = 1, n do
            if words[k] == w then dup = true break end
        end
        if not dup then
            n = n + 1
            words[n] = w
        end
    end
    return table.concat(words, " ")
end

local function gen_type()
    return pick(T1) .. " " .. pick(T2) .. " " .. pick(T3)
end

local function gen_phone(nation_key)
    return string.format("%d-%03d-%03d-%04d",
        10 + nation_key, rand_int(100, 999), rand_int(100, 999), rand_int(1000, 9999))
end

-- Money is kept in integer cents, as dbgen keeps it, and printed as two
-- integers: glibc's float formatting was a sixth of the load's CPU.
-- dbgen's retail price of a part, in cents (rpb_routine): 900.00 to 2099.00.
-- lineitem derives its extended price from the same function.
local function rpb(lp)
    return 90000 + (lp // 10) % 20001 + (lp % 1000) * 100
end

-- Cents as a money string; the account balances can be negative.
local function money(c)
    if c < 0 then
        return string.format("-%d.%02d", (-c) // 100, (-c) % 100)
    end
    return string.format("%d.%02d", c // 100, c % 100)
end

-- dbgen's PART_SUPP_BRIDGE: the i-th (0..3) supplier of a part, within the
-- unit. partsupp and lineitem both use it, so (l_partkey, l_suppkey) always
-- exists in partsupp.
local function supplier_of(lp, i)
    return ((lp + i * (SUPPLIERS_PER_UNIT // 4 + (lp - 1) // SUPPLIERS_PER_UNIT)) % SUPPLIERS_PER_UNIT) + 1
end

-- Replace `#marker` characters of a comment at offset `off` (0-based).
local function splice(comment, off, marker)
    return string.sub(comment, 1, off) .. marker .. string.sub(comment, off + #marker + 1)
end

-- Pure-Lua date arithmetic (no SQL round-trips)
local function ymd_to_days(y, m, d)
    local a = math.floor((14 - m) / 12)
    local y2 = y + 4800 - a
    local m2 = m + 12 * a - 3
    return d + math.floor((153 * m2 + 2) / 5) + 365 * y2
        + math.floor(y2 / 4) - math.floor(y2 / 100) + math.floor(y2 / 400) - 32045
end

local function days_to_ymd(jd)
    local a = jd + 32044
    local b = math.floor((4 * a + 3) / 146097)
    local c = a - math.floor(146097 * b / 4)
    local d = math.floor((4 * c + 3) / 1461)
    local e = c - math.floor(1461 * d / 4)
    local m = math.floor((5 * e + 2) / 153)
    local day = e - math.floor((153 * m + 2) / 5) + 1
    local month = m + 3 - 12 * math.floor(m / 10)
    local year = 100 * b + d - 4800 + math.floor(m / 10)
    return year, month, day
end

-- dbgen's calendar: orders are dated 1992-01-01 to 1998-08-02 (2,406 days)
-- so that the latest ship date is 1998-12-01 and the latest receipt date
-- 1998-12-31; "today" is 1995-06-17. Dates are handled as day numbers and
-- formatted once, through this table.
local JD_START = ymd_to_days(1992, 1, 1)
local JD_CURRENT = ymd_to_days(1995, 6, 17)
local ORDER_DAYS = 2406
local DATES = {}
for jd = JD_START, JD_START + ORDER_DAYS + 121 + 30 do
    local y, m, d = days_to_ymd(jd)
    DATES[jd - JD_START + 1] = string.format("%04d-%02d-%02d", y, m, d)
end
local function date_of(jd) return DATES[jd - JD_START + 1] end

-- Pre-allocate reusable buffer tables
local buf = {}
local obuf = {}
local lbuf = {}
local lines = {}

-- Helper: flush n entries from buffer to COPY
local function flush_copy(sql, t, n)
    copy_start(sql)
    copy_write(table.concat(t, "", 1, n))
    copy_end()
end

-- ============================================================
-- One unit == one scalefactor's worth of data. Keys are offset
-- so jobs loading different units never collide.
-- ============================================================
for _, unit in ipairs(ids) do
    local part_off = (unit - 1) * PARTS_PER_UNIT
    local supp_off = (unit - 1) * SUPPLIERS_PER_UNIT
    local cust_off = (unit - 1) * CUSTOMERS_PER_UNIT
    local ord_off  = (unit - 1) * ORDERS_PER_UNIT
    local clerk_off = (unit - 1) * CLERKS_PER_UNIT

    -- --------------------------------------------------------
    -- 1. Part. The brand's first digit is the manufacturer's.
    -- --------------------------------------------------------
    local n_batches = math.ceil(PARTS_PER_UNIT / BATCH)
    for b = 1, n_batches do
        local s = (b - 1) * BATCH + 1
        local e = math.min(b * BATCH, PARTS_PER_UNIT)
        local cnt = e - s + 1
        local comments = rand_strings(cnt, 5, 22)
        local n = 0
        for lp = s, e do
            n = n + 1
            local mfgr = rand_int(1, 5)
            local rp = rpb(lp)
            buf[n] = string.format("%d\t%s\tManufacturer#%d\tBrand#%d%d\t%s\t%d\t%s\t%d.%02d\t%s\n",
                part_off + lp, gen_part_name(), mfgr, mfgr, rand_int(1, 5),
                gen_type(), rand_int(1, 50), pick(CONTAINERS),
                rp // 100, rp % 100, comments[n])
        end
        flush_copy("COPY part FROM stdin", buf, n)
    end

    -- --------------------------------------------------------
    -- 2. Supplier. Ten comments per unit carry "Customer ...
    --    Complaints" or "Customer ... Recommends", as dbgen plants
    --    them; Q16 excludes the complained-about suppliers.
    -- --------------------------------------------------------
    n_batches = math.ceil(SUPPLIERS_PER_UNIT / SUPPLIER_BATCH)
    for b = 1, n_batches do
        local s = (b - 1) * SUPPLIER_BATCH + 1
        local e = math.min(b * SUPPLIER_BATCH, SUPPLIERS_PER_UNIT)
        local cnt = e - s + 1
        local addrs = rand_strings(cnt, 10, 40)
        local comments = rand_strings(cnt, 25, 100)
        local n = 0
        for ls = s, e do
            n = n + 1
            local gk = supp_off + ls
            local nk = rand_int(0, 24)
            local comment = comments[n]
            if rand_int(1, 10000) <= 10 then
                local kind = rand_int(0, 100) < 50 and "Complaints" or "Recommends"
                local noise = rand_int(0, #comment - 19)
                local off = rand_int(0, #comment - 19 - noise)
                comment = splice(splice(comment, off, "Customer "), off + 9 + noise, kind)
            end
            buf[n] = string.format("%d\tSupplier#%09d\t%s\t%d\t%s\t%s\t%s\n",
                gk, gk, addrs[n], nk, gen_phone(nk),
                money(rand_int(-99999, 999999)), comment)
        end
        flush_copy("COPY supplier FROM stdin", buf, n)
    end

    -- --------------------------------------------------------
    -- 3. Partsupp (4 per part)
    -- --------------------------------------------------------
    n_batches = math.ceil(PARTS_PER_UNIT / BATCH)
    for b = 1, n_batches do
        local s = (b - 1) * BATCH + 1
        local e = math.min(b * BATCH, PARTS_PER_UNIT)
        local cnt = (e - s + 1) * PARTSUPPS_PER_PART
        local comments = rand_strings(cnt, 49, 198)
        local n = 0
        for lp = s, e do
            for i = 0, PARTSUPPS_PER_PART - 1 do
                n = n + 1
                local qty = rand_int(1, 9999)
                local cost = rand_int(100, 100000)
                buf[n] = string.format("%d\t%d\t%d\t%d.%02d\t%s\n",
                    part_off + lp, supp_off + supplier_of(lp, i), qty,
                    cost // 100, cost % 100, comments[n])
            end
        end
        flush_copy("COPY partsupp FROM stdin", buf, n)
    end

    -- --------------------------------------------------------
    -- 4. Customer
    -- --------------------------------------------------------
    n_batches = math.ceil(CUSTOMERS_PER_UNIT / BATCH)
    for b = 1, n_batches do
        local s = (b - 1) * BATCH + 1
        local e = math.min(b * BATCH, CUSTOMERS_PER_UNIT)
        local cnt = e - s + 1
        local addrs = rand_strings(cnt, 10, 40)
        local comments = rand_strings(cnt, 29, 116)
        local n = 0
        for lc = s, e do
            n = n + 1
            local gk = cust_off + lc
            local nk = rand_int(0, 24)
            buf[n] = string.format("%d\tCustomer#%09d\t%s\t%d\t%s\t%s\t%s\t%s\n",
                gk, gk, addrs[n], nk, gen_phone(nk),
                money(rand_int(-99999, 999999)), pick(SEGMENTS), comments[n])
        end
        flush_copy("COPY customer FROM stdin", buf, n)
    end

    -- --------------------------------------------------------
    -- 5. Orders + Lineitem
    -- --------------------------------------------------------
    n_batches = math.ceil(ORDERS_PER_UNIT / BATCH)
    for b = 1, n_batches do
        local s = (b - 1) * BATCH + 1
        local e = math.min(b * BATCH, ORDERS_PER_UNIT)
        local cnt = e - s + 1

        -- The line counts first, so the comments are drawn for the lines
        -- that exist.
        local total_lines = 0
        for k = 1, cnt do
            lines[k] = rand_int(1, 7)
            total_lines = total_lines + lines[k]
        end
        local o_comments = rand_strings(cnt, 19, 78)
        local l_comments = rand_strings(total_lines, 10, 43)
        local lc_idx = 0

        local on = 0
        local ln = 0

        for lok = s, e do
            -- Customer mortality: keys divisible by 3 never order; dbgen
            -- moves them to the next key, or the previous one at the top.
            local o_ck = rand_int(1, CUSTOMERS_PER_UNIT)
            if o_ck % 3 == 0 then
                o_ck = (o_ck < CUSTOMERS_PER_UNIT) and o_ck + 1 or o_ck - 1
            end
            local o_jd = JD_START + rand_int(0, ORDER_DAYS - 1)
            local o_priority = pick(PRIORITIES)
            local o_clerk = clerk_off + rand_int(1, CLERKS_PER_UNIT)

            on = on + 1
            local n_lines = lines[on]
            local o_totalprice = 0 -- cents
            local shipped = 0

            for li = 1, n_lines do
                ln = ln + 1
                lc_idx = lc_idx + 1
                local l_pk = rand_int(1, PARTS_PER_UNIT)
                -- The small draws of a line come from three calls into the
                -- engine instead of ten, decoded as mixed-radix digits: each
                -- component is still uniform and independent of the others.
                local r = rand_int(0, 4949)          -- quantity 50 x discount 11 x tax 9
                local l_qty = r % 50 + 1
                r = r // 50
                local l_disc = r % 11                -- hundredths: 0.00 to 0.10
                local l_tax = r // 11                -- hundredths: 0.00 to 0.08
                local l_extprice = rpb(l_pk) * l_qty -- cents
                -- dbgen's running total: integer division after the discount
                -- and again after the tax.
                o_totalprice = o_totalprice
                    + (l_extprice * (100 - l_disc) // 100) * (100 + l_tax) // 100

                r = rand_int(0, 221429)              -- ship 121 x commit 61 x receipt 30
                local s_jd = o_jd + 1 + r % 121
                r = r // 121
                local c_jd = o_jd + 30 + r % 61
                local r_jd = s_jd + 1 + r // 61

                r = rand_int(0, 223)                 -- supplier 4 x instruct 4 x mode 7 x flag 2
                local l_sk = supplier_of(l_pk, r % 4)
                r = r // 4
                local l_instruct = SHIP_INSTRUCT[r % 4 + 1]
                r = r // 4
                local l_mode = SHIP_MODES[r % 7 + 1]
                local l_rflag = "N"
                if r_jd <= JD_CURRENT then
                    l_rflag = r // 7 == 0 and "R" or "A"
                end
                local l_lstatus = "O"
                if s_jd <= JD_CURRENT then
                    l_lstatus = "F"
                    shipped = shipped + 1
                end

                lbuf[ln] = string.format(
                    "%d\t%d\t%d\t%d\t%d\t%d.%02d\t0.%02d\t0.%02d\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n",
                    ord_off + lok, part_off + l_pk, supp_off + l_sk, li, l_qty,
                    l_extprice // 100, l_extprice % 100, l_disc, l_tax,
                    l_rflag, l_lstatus, date_of(s_jd), date_of(c_jd), date_of(r_jd),
                    l_instruct, l_mode, l_comments[lc_idx])
            end

            -- All lines shipped by the current date: F; some: P; none: O.
            local o_status = "O"
            if shipped == n_lines then
                o_status = "F"
            elseif shipped > 0 then
                o_status = "P"
            end
            local o_comment = o_comments[on]
            local r = rand_int(1, 64)
            if r <= 16 then
                local pair = Q13_ADJ[(r - 1) % 4 + 1] .. " " .. Q13_NOUN[(r - 1) // 4 + 1]
                o_comment = splice(o_comment, rand_int(0, #o_comment - #pair), pair)
            end
            obuf[on] = string.format("%d\t%d\t%s\t%d.%02d\t%s\t%s\tClerk#%09d\t0\t%s\n",
                ord_off + lok, cust_off + o_ck, o_status, o_totalprice // 100, o_totalprice % 100,
                date_of(o_jd), o_priority, o_clerk, o_comment)
        end

        -- Flush orders
        copy_start("COPY orders FROM stdin")
        copy_write(table.concat(obuf, "", 1, on))
        copy_end()

        -- Flush lineitem
        copy_start("COPY lineitem FROM stdin")
        copy_write(table.concat(lbuf, "", 1, ln))
        copy_end()
    end
end
