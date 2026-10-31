-- PGM-H Query Implementations
-- This file is loaded once per transaction ID. The script body runs the query
-- corresponding to the transaction ID that was dispatched to it.
-- Since we don't know which ID we are at script level, we define all queries
-- as a lookup table and use a global __current_tx_id set by the executor.
--
-- However, the current architecture wraps this file as:
--   function __tx_N() <this content> end
-- So each transaction gets its own copy. We use the query ID from the
-- transaction's position to dispatch.
--
-- WORKAROUND: Since all 22 transactions point to this same file,
-- each __tx_N function contains the full dispatch table. We detect which
-- query to run via a lightweight approach: each function runs all the code,
-- but we use the fact that rand_int/query/exec are available to just execute
-- the query inline. The overhead of the unused dispatch code is negligible.

-- Reference data for random parameter generation
local REGIONS = {"AFRICA", "AMERICA", "ASIA", "EUROPE", "MIDDLE EAST"}
local NATIONS = {
    "ALGERIA", "ARGENTINA", "BRAZIL", "CANADA", "EGYPT",
    "ETHIOPIA", "FRANCE", "GERMANY", "INDIA", "INDONESIA",
    "IRAN", "IRAQ", "JAPAN", "JORDAN", "KENYA",
    "MOROCCO", "MOZAMBIQUE", "PERU", "CHINA", "ROMANIA",
    "SAUDI ARABIA", "VIETNAM", "RUSSIA", "UNITED KINGDOM", "UNITED STATES"
}
local SEGMENTS = {"AUTOMOBILE", "BUILDING", "FURNITURE", "HOUSEHOLD", "MACHINERY"}
local SHIP_MODES = {"REG AIR", "AIR", "RAIL", "SHIP", "TRUCK", "MAIL", "FOB"}
local TYPES1 = {"STANDARD", "SMALL", "MEDIUM", "LARGE", "ECONOMY", "PROMO"}
local TYPES2 = {"ANODIZED", "BURNISHED", "PLATED", "POLISHED", "BRUSHED"}
local TYPES3 = {"TIN", "NICKEL", "BRASS", "STEEL", "COPPER"}
local COLORS = {"almond", "antique", "aquamarine", "azure", "beige", "bisque", "black", "blanched", "blue"}

local function pick(t) return t[rand_int(1, #t)] end
local function rand_date(y1, y2)
    local y = rand_int(y1, y2)
    local m = rand_int(1, 12)
    local d = rand_int(1, 28)
    return string.format("%04d-%02d-%02d", y, m, d)
end
local function first_of_month(y1, y2)
    return string.format("%04d-%02d-01", rand_int(y1, y2), rand_int(1, 12))
end
local function first_of_year(y1, y2)
    return string.format("%04d-01-01", rand_int(y1, y2))
end
local function gen_type()
    return pick(TYPES1) .. " " .. pick(TYPES2) .. " " .. pick(TYPES3)
end
local function gen_brand()
    return string.format("Brand#%d%d", rand_int(1,5), rand_int(1,5))
end

-- Build dispatch table: tx_id -> function (global, accessible from inline scripts)
Q = {}

Q[1] = function() -- Pricing Summary
    local delta = rand_int(60, 120)
    exec("SELECT l_returnflag, l_linestatus, SUM(l_quantity), SUM(l_extendedprice), "
        .. "SUM(l_extendedprice * (1 - l_discount)), SUM(l_extendedprice * (1 - l_discount) * (1 + l_tax)), "
        .. "AVG(l_quantity), AVG(l_extendedprice), AVG(l_discount), COUNT(*) "
        .. "FROM lineitem WHERE l_shipdate <= DATE '1998-12-01' - INTERVAL '" .. delta .. " day' "
        .. "GROUP BY l_returnflag, l_linestatus ORDER BY l_returnflag, l_linestatus")
end

Q[2] = function() -- Min Cost Supplier
    local size = rand_int(1, 50)
    local tp = pick(TYPES3)
    local region = pick(REGIONS)
    exec("SELECT s_acctbal, s_name, n_name, p_partkey, p_mfgr, s_address, s_phone, s_comment "
        .. "FROM part, supplier, partsupp, nation, region "
        .. "WHERE p_partkey = ps_partkey AND s_suppkey = ps_suppkey "
        .. "AND p_size = " .. size .. " AND p_type LIKE '%" .. tp .. "' "
        .. "AND s_nationkey = n_nationkey AND n_regionkey = r_regionkey AND r_name = '" .. region .. "' "
        .. "AND ps_supplycost = (SELECT MIN(ps_supplycost) FROM partsupp, supplier, nation, region "
        .. "WHERE p_partkey = ps_partkey AND s_suppkey = ps_suppkey AND s_nationkey = n_nationkey "
        .. "AND n_regionkey = r_regionkey AND r_name = '" .. region .. "') "
        .. "ORDER BY s_acctbal DESC, n_name, s_name, p_partkey LIMIT 100")
end

Q[3] = function() -- Shipping Priority
    local seg = pick(SEGMENTS)
    local dt = rand_date(1995, 1995)
    exec("SELECT l_orderkey, SUM(l_extendedprice * (1 - l_discount)) AS revenue, o_orderdate, o_shippriority "
        .. "FROM customer, orders, lineitem WHERE c_mktsegment = '" .. seg .. "' AND c_custkey = o_custkey "
        .. "AND l_orderkey = o_orderkey AND o_orderdate < DATE '" .. dt .. "' AND l_shipdate > DATE '" .. dt .. "' "
        .. "GROUP BY l_orderkey, o_orderdate, o_shippriority ORDER BY revenue DESC, o_orderdate LIMIT 10")
end

Q[4] = function() -- Order Priority
    local dt = first_of_month(1993, 1997)
    exec("SELECT o_orderpriority, COUNT(*) FROM orders "
        .. "WHERE o_orderdate >= DATE '" .. dt .. "' AND o_orderdate < DATE '" .. dt .. "' + INTERVAL '3 month' "
        .. "AND EXISTS (SELECT * FROM lineitem WHERE l_orderkey = o_orderkey AND l_commitdate < l_receiptdate) "
        .. "GROUP BY o_orderpriority ORDER BY o_orderpriority")
end

Q[5] = function() -- Local Supplier Volume
    local region = pick(REGIONS)
    local dt = first_of_year(1993, 1997)
    exec("SELECT n_name, SUM(l_extendedprice * (1 - l_discount)) AS revenue "
        .. "FROM customer, orders, lineitem, supplier, nation, region "
        .. "WHERE c_custkey = o_custkey AND l_orderkey = o_orderkey AND l_suppkey = s_suppkey "
        .. "AND c_nationkey = s_nationkey AND s_nationkey = n_nationkey AND n_regionkey = r_regionkey "
        .. "AND r_name = '" .. region .. "' AND o_orderdate >= DATE '" .. dt .. "' "
        .. "AND o_orderdate < DATE '" .. dt .. "' + INTERVAL '1 year' "
        .. "GROUP BY n_name ORDER BY revenue DESC")
end

Q[6] = function() -- Revenue Forecast
    local dt = first_of_year(1993, 1997)
    local disc = rand_int(2, 9) / 100.0
    local qty = rand_int(24, 25)
    exec(string.format("SELECT SUM(l_extendedprice * l_discount) FROM lineitem "
        .. "WHERE l_shipdate >= DATE '%s' AND l_shipdate < DATE '%s' + INTERVAL '1 year' "
        .. "AND l_discount BETWEEN %.2f - 0.01 AND %.2f + 0.01 AND l_quantity < %d",
        dt, dt, disc, disc, qty))
end

Q[7] = function() -- Volume Shipping
    local n1 = pick(NATIONS)
    local n2 = n1
    while n2 == n1 do n2 = pick(NATIONS) end
    exec("SELECT supp_nation, cust_nation, l_year, SUM(volume) AS revenue FROM ("
        .. "SELECT n1.n_name AS supp_nation, n2.n_name AS cust_nation, EXTRACT(YEAR FROM l_shipdate) AS l_year, "
        .. "l_extendedprice * (1 - l_discount) AS volume "
        .. "FROM supplier, lineitem, orders, customer, nation n1, nation n2 "
        .. "WHERE s_suppkey = l_suppkey AND o_orderkey = l_orderkey AND c_custkey = o_custkey "
        .. "AND s_nationkey = n1.n_nationkey AND c_nationkey = n2.n_nationkey "
        .. "AND ((n1.n_name = '" .. n1 .. "' AND n2.n_name = '" .. n2 .. "') "
        .. "OR (n1.n_name = '" .. n2 .. "' AND n2.n_name = '" .. n1 .. "')) "
        .. "AND l_shipdate BETWEEN DATE '1995-01-01' AND DATE '1996-12-31') AS shipping "
        .. "GROUP BY supp_nation, cust_nation, l_year ORDER BY supp_nation, cust_nation, l_year")
end

Q[8] = function() -- Market Share
    local nation = pick(NATIONS)
    local region = pick(REGIONS)
    local ptype = gen_type()
    exec("SELECT o_year, SUM(CASE WHEN nation = '" .. nation .. "' THEN volume ELSE 0 END) / SUM(volume) FROM ("
        .. "SELECT EXTRACT(YEAR FROM o_orderdate) AS o_year, l_extendedprice * (1 - l_discount) AS volume, "
        .. "n2.n_name AS nation FROM part, supplier, lineitem, orders, customer, nation n1, nation n2, region "
        .. "WHERE p_partkey = l_partkey AND s_suppkey = l_suppkey AND l_orderkey = o_orderkey "
        .. "AND o_custkey = c_custkey AND c_nationkey = n1.n_nationkey AND n1.n_regionkey = r_regionkey "
        .. "AND r_name = '" .. region .. "' AND s_nationkey = n2.n_nationkey "
        .. "AND o_orderdate BETWEEN DATE '1995-01-01' AND DATE '1996-12-31' "
        .. "AND p_type = '" .. ptype .. "') AS all_nations GROUP BY o_year ORDER BY o_year")
end

Q[9] = function() -- Product Profit
    local color = pick(COLORS)
    exec("SELECT nation, o_year, SUM(amount) FROM ("
        .. "SELECT n_name AS nation, EXTRACT(YEAR FROM o_orderdate) AS o_year, "
        .. "l_extendedprice * (1 - l_discount) - ps_supplycost * l_quantity AS amount "
        .. "FROM part, supplier, lineitem, partsupp, orders, nation "
        .. "WHERE s_suppkey = l_suppkey AND ps_suppkey = l_suppkey AND ps_partkey = l_partkey "
        .. "AND p_partkey = l_partkey AND o_orderkey = l_orderkey AND s_nationkey = n_nationkey "
        .. "AND p_name LIKE '%" .. color .. "%') AS profit GROUP BY nation, o_year ORDER BY nation, o_year DESC")
end

Q[10] = function() -- Returned Items
    local dt = first_of_month(1993, 1995)
    exec("SELECT c_custkey, c_name, SUM(l_extendedprice * (1 - l_discount)) AS revenue, "
        .. "c_acctbal, n_name, c_address, c_phone, c_comment "
        .. "FROM customer, orders, lineitem, nation WHERE c_custkey = o_custkey AND l_orderkey = o_orderkey "
        .. "AND o_orderdate >= DATE '" .. dt .. "' AND o_orderdate < DATE '" .. dt .. "' + INTERVAL '3 month' "
        .. "AND l_returnflag = 'R' AND c_nationkey = n_nationkey "
        .. "GROUP BY c_custkey, c_name, c_acctbal, c_phone, n_name, c_address, c_comment "
        .. "ORDER BY revenue DESC LIMIT 20")
end

Q[11] = function() -- Important Stock
    local nation = pick(NATIONS)
    local fraction = 0.0001 / (scalefactor > 0 and scalefactor or 1)
    exec(string.format("SELECT ps_partkey, SUM(ps_supplycost * ps_availqty) AS value "
        .. "FROM partsupp, supplier, nation WHERE ps_suppkey = s_suppkey AND s_nationkey = n_nationkey "
        .. "AND n_name = '%s' GROUP BY ps_partkey "
        .. "HAVING SUM(ps_supplycost * ps_availqty) > ("
        .. "SELECT SUM(ps_supplycost * ps_availqty) * %f FROM partsupp, supplier, nation "
        .. "WHERE ps_suppkey = s_suppkey AND s_nationkey = n_nationkey AND n_name = '%s') "
        .. "ORDER BY value DESC", nation, fraction, nation))
end

Q[12] = function() -- Shipping Modes
    local m1 = pick(SHIP_MODES)
    local m2 = m1
    while m2 == m1 do m2 = pick(SHIP_MODES) end
    local dt = first_of_year(1993, 1997)
    exec("SELECT l_shipmode, "
        .. "SUM(CASE WHEN o_orderpriority = '1-URGENT' OR o_orderpriority = '2-HIGH' THEN 1 ELSE 0 END), "
        .. "SUM(CASE WHEN o_orderpriority <> '1-URGENT' AND o_orderpriority <> '2-HIGH' THEN 1 ELSE 0 END) "
        .. "FROM orders, lineitem WHERE o_orderkey = l_orderkey AND l_shipmode IN ('" .. m1 .. "', '" .. m2 .. "') "
        .. "AND l_commitdate < l_receiptdate AND l_shipdate < l_commitdate "
        .. "AND l_receiptdate >= DATE '" .. dt .. "' AND l_receiptdate < DATE '" .. dt .. "' + INTERVAL '1 year' "
        .. "GROUP BY l_shipmode ORDER BY l_shipmode")
end

Q[13] = function() -- Customer Distribution
    local w1 = ({"special", "pending", "unusual", "express"})[rand_int(1,4)]
    local w2 = ({"packages", "requests", "accounts", "deposits"})[rand_int(1,4)]
    exec("SELECT c_count, COUNT(*) AS custdist FROM ("
        .. "SELECT c_custkey, COUNT(o_orderkey) AS c_count FROM customer LEFT OUTER JOIN orders "
        .. "ON c_custkey = o_custkey AND o_comment NOT LIKE '%" .. w1 .. "%" .. w2 .. "%' "
        .. "GROUP BY c_custkey) AS c_orders GROUP BY c_count ORDER BY custdist DESC, c_count DESC")
end

Q[14] = function() -- Promotion Effect
    local dt = first_of_month(1993, 1997)
    exec("SELECT 100.00 * SUM(CASE WHEN p_type LIKE 'PROMO%' THEN l_extendedprice * (1 - l_discount) ELSE 0 END) "
        .. "/ SUM(l_extendedprice * (1 - l_discount)) FROM lineitem, part "
        .. "WHERE l_partkey = p_partkey AND l_shipdate >= DATE '" .. dt .. "' "
        .. "AND l_shipdate < DATE '" .. dt .. "' + INTERVAL '1 month'")
end

Q[15] = function() -- Top Supplier
    local dt = first_of_month(1993, 1997)
    exec("WITH revenue AS (SELECT l_suppkey AS supplier_no, SUM(l_extendedprice * (1 - l_discount)) AS total_revenue "
        .. "FROM lineitem WHERE l_shipdate >= DATE '" .. dt .. "' AND l_shipdate < DATE '" .. dt .. "' + INTERVAL '3 month' "
        .. "GROUP BY l_suppkey) SELECT s_suppkey, s_name, s_address, s_phone, total_revenue "
        .. "FROM supplier, revenue WHERE s_suppkey = supplier_no "
        .. "AND total_revenue = (SELECT MAX(total_revenue) FROM revenue) ORDER BY s_suppkey")
end

Q[16] = function() -- Parts/Supplier
    local brand = gen_brand()
    local ptype = pick(TYPES1) .. " " .. pick(TYPES2)
    local sizes = {}
    for k = 1, 8 do sizes[k] = rand_int(1, 50) end
    exec("SELECT p_brand, p_type, p_size, COUNT(DISTINCT ps_suppkey) FROM partsupp, part "
        .. "WHERE p_partkey = ps_partkey AND p_brand <> '" .. brand .. "' AND p_type NOT LIKE '" .. ptype .. "%' "
        .. "AND p_size IN (" .. table.concat(sizes, ",") .. ") "
        .. "AND ps_suppkey NOT IN (SELECT s_suppkey FROM supplier WHERE s_comment LIKE '%Customer%Complaints%') "
        .. "GROUP BY p_brand, p_type, p_size ORDER BY COUNT(DISTINCT ps_suppkey) DESC, p_brand, p_type, p_size")
end

Q[17] = function() -- Small Quantity
    local brand = gen_brand()
    local containers = {"SM CASE","SM BOX","SM PACK","SM PKG","SM JAR","SM BAG","SM CAN","SM DRUM",
        "MED CASE","MED BOX","MED PACK","MED PKG","MED JAR","MED BAG","MED CAN","MED DRUM",
        "LG CASE","LG BOX","LG PACK","LG PKG","LG JAR","LG BAG","LG CAN","LG DRUM",
        "JUMBO CASE","JUMBO BOX","JUMBO PACK","JUMBO PKG","JUMBO JAR","JUMBO BAG","JUMBO CAN","JUMBO DRUM",
        "WRAP CASE","WRAP BOX","WRAP PACK","WRAP PKG","WRAP JAR","WRAP BAG","WRAP CAN","WRAP DRUM"}
    local container = pick(containers)
    exec("SELECT SUM(l_extendedprice) / 7.0 FROM lineitem, part "
        .. "WHERE p_partkey = l_partkey AND p_brand = '" .. brand .. "' AND p_container = '" .. container .. "' "
        .. "AND l_quantity < (SELECT 0.2 * AVG(l_quantity) FROM lineitem WHERE l_partkey = p_partkey)")
end

Q[18] = function() -- Large Volume
    local qty = rand_int(312, 315)
    exec("SELECT c_name, c_custkey, o_orderkey, o_orderdate, o_totalprice, SUM(l_quantity) "
        .. "FROM customer, orders, lineitem WHERE o_orderkey IN ("
        .. "SELECT l_orderkey FROM lineitem GROUP BY l_orderkey HAVING SUM(l_quantity) > " .. qty .. ") "
        .. "AND c_custkey = o_custkey AND o_orderkey = l_orderkey "
        .. "GROUP BY c_name, c_custkey, o_orderkey, o_orderdate, o_totalprice "
        .. "ORDER BY o_totalprice DESC, o_orderdate LIMIT 100")
end

Q[19] = function() -- Discounted Revenue
    local b1, b2, b3 = gen_brand(), gen_brand(), gen_brand()
    local q1, q2, q3 = rand_int(1,10), rand_int(10,20), rand_int(20,30)
    exec(string.format("SELECT SUM(l_extendedprice * (1 - l_discount)) FROM lineitem, part "
        .. "WHERE (p_partkey = l_partkey AND p_brand = '%s' "
        .. "AND p_container IN ('SM CASE','SM BOX','SM PACK','SM PKG') "
        .. "AND l_quantity >= %d AND l_quantity <= %d + 10 AND p_size BETWEEN 1 AND 5 "
        .. "AND l_shipmode IN ('AIR','AIR REG') AND l_shipinstruct = 'DELIVER IN PERSON') "
        .. "OR (p_partkey = l_partkey AND p_brand = '%s' "
        .. "AND p_container IN ('MED BAG','MED BOX','MED PKG','MED PACK') "
        .. "AND l_quantity >= %d AND l_quantity <= %d + 10 AND p_size BETWEEN 1 AND 10 "
        .. "AND l_shipmode IN ('AIR','AIR REG') AND l_shipinstruct = 'DELIVER IN PERSON') "
        .. "OR (p_partkey = l_partkey AND p_brand = '%s' "
        .. "AND p_container IN ('LG CASE','LG BOX','LG PACK','LG PKG') "
        .. "AND l_quantity >= %d AND l_quantity <= %d + 10 AND p_size BETWEEN 1 AND 15 "
        .. "AND l_shipmode IN ('AIR','AIR REG') AND l_shipinstruct = 'DELIVER IN PERSON')",
        b1, q1, q1, b2, q2, q2, b3, q3, q3))
end

Q[20] = function() -- Potential Promotion
    local color = pick(COLORS)
    local dt = first_of_year(1993, 1997)
    local nation = pick(NATIONS)
    exec("SELECT s_name, s_address FROM supplier, nation "
        .. "WHERE s_suppkey IN (SELECT ps_suppkey FROM partsupp "
        .. "WHERE ps_partkey IN (SELECT p_partkey FROM part WHERE p_name LIKE '" .. color .. "%') "
        .. "AND ps_availqty > (SELECT 0.5 * SUM(l_quantity) FROM lineitem "
        .. "WHERE l_partkey = ps_partkey AND l_suppkey = ps_suppkey "
        .. "AND l_shipdate >= DATE '" .. dt .. "' AND l_shipdate < DATE '" .. dt .. "' + INTERVAL '1 year')) "
        .. "AND s_nationkey = n_nationkey AND n_name = '" .. nation .. "' ORDER BY s_name")
end

Q[21] = function() -- Waiting Supplier
    local nation = pick(NATIONS)
    exec("SELECT s_name, COUNT(*) AS numwait FROM supplier, lineitem l1, orders, nation "
        .. "WHERE s_suppkey = l1.l_suppkey AND o_orderkey = l1.l_orderkey AND o_orderstatus = 'F' "
        .. "AND l1.l_receiptdate > l1.l_commitdate "
        .. "AND EXISTS (SELECT * FROM lineitem l2 WHERE l2.l_orderkey = l1.l_orderkey AND l2.l_suppkey <> l1.l_suppkey) "
        .. "AND NOT EXISTS (SELECT * FROM lineitem l3 WHERE l3.l_orderkey = l1.l_orderkey "
        .. "AND l3.l_suppkey <> l1.l_suppkey AND l3.l_receiptdate > l3.l_commitdate) "
        .. "AND s_nationkey = n_nationkey AND n_name = '" .. nation .. "' "
        .. "GROUP BY s_name ORDER BY numwait DESC, s_name LIMIT 100")
end

Q[22] = function() -- Global Sales
    local codes = {}
    while #codes < 7 do
        local c = rand_int(10, 34)
        local found = false
        for _, v in ipairs(codes) do if v == c then found = true end end
        if not found then codes[#codes+1] = c end
    end
    local codes_str = "'" .. table.concat(codes, "','") .. "'"
    exec("SELECT cntrycode, COUNT(*), SUM(c_acctbal) FROM ("
        .. "SELECT SUBSTRING(c_phone FROM 1 FOR 2) AS cntrycode, c_acctbal FROM customer "
        .. "WHERE SUBSTRING(c_phone FROM 1 FOR 2) IN (" .. codes_str .. ") "
        .. "AND c_acctbal > (SELECT AVG(c_acctbal) FROM customer WHERE c_acctbal > 0.00 "
        .. "AND SUBSTRING(c_phone FROM 1 FOR 2) IN (" .. codes_str .. ")) "
        .. "AND NOT EXISTS (SELECT * FROM orders WHERE o_custkey = c_custkey)) AS custsale "
        .. "GROUP BY cntrycode ORDER BY cntrycode")
end

-- Q is defined at global scope (this file is loaded as a module).
-- Each transaction uses inline script: "Q[N]()" to dispatch.
