-- PGM-DS Query Workload (Phase 3) -- representative subset.
--
-- Loaded once per thread as a shared module; defines the global dispatch table
-- Q[1]..Q[N]. Each transaction in spec.yaml runs `Q[n]()` via an inline script.
--
-- These are shaped after representative decision-support query templates but simplified to this
-- port's schema and data ranges (sales fall in d_year 1998..2002; inventory
-- snapshots span ~1998..2000). They exercise the star-schema join/aggregate
-- patterns across every channel -- the access paths a custom Table Access
-- Method needs stressed -- rather than reproducing the exact 99-query text.

local CATEGORIES = {"Women", "Men", "Children", "Shoes", "Music", "Jewelry",
    "Home", "Sports", "Books", "Electronics"}
local GENDER = {"M", "F"}
local MARITAL = {"M", "S", "D", "W", "U"}
local EDU = {"Primary", "Secondary", "College", "2 yr Degree",
    "4 yr Degree", "Advanced Degree", "Unknown"}

local function pick(t) return t[rand_int(1, #t)] end
local function ryear() return rand_int(1998, 2002) end
local function rmoy() return rand_int(1, 12) end

Q = {}

-- Q1: store sales by brand for one manufacturer in a month.
Q[1] = function()
    exec(string.format(
        "SELECT dt.d_year, i.i_brand_id, i.i_brand, SUM(ss.ss_ext_sales_price) sum_agg "
        .. "FROM date_dim dt, store_sales ss, item i "
        .. "WHERE dt.d_date_sk = ss.ss_sold_date_sk AND ss.ss_item_sk = i.i_item_sk "
        .. "AND i.i_manufact_id = %d AND dt.d_moy = %d "
        .. "GROUP BY dt.d_year, i.i_brand, i.i_brand_id "
        .. "ORDER BY dt.d_year, sum_agg DESC, i.i_brand_id LIMIT 100",
        rand_int(1, 9), rmoy()))
end

-- Q2: store sales by category/year for one manager+month.
Q[2] = function()
    exec(string.format(
        "SELECT dt.d_year, i.i_category_id, i.i_category, SUM(ss.ss_ext_sales_price) ext "
        .. "FROM date_dim dt, store_sales ss, item i "
        .. "WHERE dt.d_date_sk = ss.ss_sold_date_sk AND ss.ss_item_sk = i.i_item_sk "
        .. "AND i.i_manager_id = %d AND dt.d_moy = %d AND dt.d_year = %d "
        .. "GROUP BY dt.d_year, i.i_category_id, i.i_category "
        .. "ORDER BY ext DESC, dt.d_year, i.i_category_id LIMIT 100",
        rand_int(1, 100), rmoy(), ryear()))
end

-- Q3: store sales by brand for one manager+month.
Q[3] = function()
    exec(string.format(
        "SELECT dt.d_year, i.i_brand_id, i.i_brand, SUM(ss.ss_ext_sales_price) ext "
        .. "FROM date_dim dt, store_sales ss, item i "
        .. "WHERE dt.d_date_sk = ss.ss_sold_date_sk AND ss.ss_item_sk = i.i_item_sk "
        .. "AND i.i_manager_id = %d AND dt.d_moy = %d AND dt.d_year = %d "
        .. "GROUP BY dt.d_year, i.i_brand, i.i_brand_id "
        .. "ORDER BY dt.d_year, ext DESC, i.i_brand_id LIMIT 100",
        rand_int(1, 100), rmoy(), ryear()))
end

-- Q4: brand revenue by manager, sold to out-of-town customers.
Q[4] = function()
    exec(string.format(
        "SELECT i.i_brand_id, i.i_brand, i.i_manufact_id, i.i_manufact, "
        .. "SUM(ss.ss_ext_sales_price) ext_price "
        .. "FROM date_dim dt, store_sales ss, item i, customer c, customer_address ca, store s "
        .. "WHERE dt.d_date_sk = ss.ss_sold_date_sk AND ss.ss_item_sk = i.i_item_sk "
        .. "AND ss.ss_customer_sk = c.c_customer_sk AND c.c_current_addr_sk = ca.ca_address_sk "
        .. "AND ss.ss_store_sk = s.s_store_sk "
        .. "AND SUBSTRING(ca.ca_zip, 1, 5) <> SUBSTRING(s.s_zip, 1, 5) "
        .. "AND i.i_manager_id = %d AND dt.d_moy = %d AND dt.d_year = %d "
        .. "GROUP BY i.i_brand, i.i_brand_id, i.i_manufact_id, i.i_manufact "
        .. "ORDER BY ext_price DESC, i.i_brand, i.i_brand_id LIMIT 100",
        rand_int(1, 100), rmoy(), ryear()))
end

-- Q5: states where buyers bought above-average-priced items.
-- The per-category average is computed once in a derived table and joined,
-- rather than as a per-row correlated subquery -- same result, but it avoids a
-- re-scan of item for every qualifying sale (which is ruinous without indexes).
Q[5] = function()
    exec(string.format(
        "SELECT ca.ca_state state, COUNT(*) cnt "
        .. "FROM customer_address ca, customer c, store_sales ss, date_dim d, item i, "
        .. "(SELECT i_category, AVG(i_current_price) avg_price FROM item GROUP BY i_category) j "
        .. "WHERE ca.ca_address_sk = c.c_current_addr_sk AND c.c_customer_sk = ss.ss_customer_sk "
        .. "AND ss.ss_sold_date_sk = d.d_date_sk AND ss.ss_item_sk = i.i_item_sk "
        .. "AND i.i_category = j.i_category "
        .. "AND d.d_year = %d AND d.d_moy = %d "
        .. "AND i.i_current_price > 1.2 * j.avg_price "
        .. "GROUP BY ca.ca_state HAVING COUNT(*) >= 10 "
        .. "ORDER BY cnt, ca.ca_state LIMIT 100",
        ryear(), rmoy()))
end

-- Q6: average sale metrics by item for a demographic + promotion.
Q[6] = function()
    exec(string.format(
        "SELECT i.i_item_id, AVG(ss.ss_quantity) agg1, AVG(ss.ss_list_price) agg2, "
        .. "AVG(ss.ss_coupon_amt) agg3, AVG(ss.ss_sales_price) agg4 "
        .. "FROM store_sales ss, customer_demographics cd, date_dim d, item i, promotion p "
        .. "WHERE ss.ss_sold_date_sk = d.d_date_sk AND ss.ss_item_sk = i.i_item_sk "
        .. "AND ss.ss_cdemo_sk = cd.cd_demo_sk AND ss.ss_promo_sk = p.p_promo_sk "
        .. "AND cd.cd_gender = '%s' AND cd.cd_marital_status = '%s' "
        .. "AND cd.cd_education_status = '%s' "
        .. "AND (p.p_channel_email = 'N' OR p.p_channel_event = 'N') AND d.d_year = %d "
        .. "GROUP BY i.i_item_id ORDER BY i.i_item_id LIMIT 100",
        pick(GENDER), pick(MARITAL), pick(EDU), ryear()))
end

-- Q7: catalog sales by call center and month.
Q[7] = function()
    exec(string.format(
        "SELECT cc.cc_name, d.d_year, d.d_moy, SUM(cs.cs_ext_sales_price) ext "
        .. "FROM catalog_sales cs, date_dim d, call_center cc "
        .. "WHERE cs.cs_sold_date_sk = d.d_date_sk AND cs.cs_call_center_sk = cc.cc_call_center_sk "
        .. "AND d.d_year = %d "
        .. "GROUP BY cc.cc_name, d.d_year, d.d_moy "
        .. "ORDER BY cc.cc_name, d.d_year, d.d_moy LIMIT 100",
        ryear()))
end

-- Q8: web sales and profit by item category.
Q[8] = function()
    exec(string.format(
        "SELECT i.i_category, d.d_year, SUM(ws.ws_ext_sales_price) ext, "
        .. "SUM(ws.ws_net_profit) profit "
        .. "FROM web_sales ws, date_dim d, item i "
        .. "WHERE ws.ws_sold_date_sk = d.d_date_sk AND ws.ws_item_sk = i.i_item_sk "
        .. "AND d.d_year = %d "
        .. "GROUP BY i.i_category, d.d_year ORDER BY ext DESC LIMIT 100",
        ryear()))
end

-- Q9: inventory quantity on hand by warehouse and category.
Q[9] = function()
    exec(string.format(
        "SELECT w.w_warehouse_name, i.i_category, SUM(inv.inv_quantity_on_hand) qoh "
        .. "FROM inventory inv, item i, warehouse w, date_dim d "
        .. "WHERE inv.inv_item_sk = i.i_item_sk AND inv.inv_warehouse_sk = w.w_warehouse_sk "
        .. "AND inv.inv_date_sk = d.d_date_sk AND d.d_year = %d AND d.d_moy = %d "
        .. "GROUP BY w.w_warehouse_name, i.i_category ORDER BY qoh DESC LIMIT 100",
        rand_int(1998, 2000), rmoy()))
end

-- Q10: customers ranked by store-return amount in a year.
Q[10] = function()
    exec(string.format(
        "SELECT c.c_customer_id, SUM(sr.sr_return_amt) ret_amt "
        .. "FROM store_returns sr, customer c, date_dim d "
        .. "WHERE sr.sr_customer_sk = c.c_customer_sk AND sr.sr_returned_date_sk = d.d_date_sk "
        .. "AND d.d_year = %d "
        .. "GROUP BY c.c_customer_id ORDER BY ret_amt DESC LIMIT 100",
        ryear()))
end

-- Q11: category sales for a single category across the three channels.
Q[11] = function()
    local cat = pick(CATEGORIES)
    exec(string.format(
        "SELECT i.i_category, AVG(ss.ss_sales_price) store_avg, COUNT(*) store_cnt "
        .. "FROM store_sales ss, item i, date_dim d "
        .. "WHERE ss.ss_item_sk = i.i_item_sk AND ss.ss_sold_date_sk = d.d_date_sk "
        .. "AND i.i_category = '%s' AND d.d_year = %d "
        .. "GROUP BY i.i_category",
        cat, ryear()))
end

-- Q12: total extended sales price per channel for one year (cross-channel).
Q[12] = function()
    local y = ryear()
    exec(string.format(
        "SELECT channel, SUM(ext) total FROM ("
        .. "SELECT 'store' channel, ss.ss_ext_sales_price ext "
        .. "FROM store_sales ss, date_dim d WHERE ss.ss_sold_date_sk = d.d_date_sk AND d.d_year = %d "
        .. "UNION ALL "
        .. "SELECT 'catalog', cs.cs_ext_sales_price "
        .. "FROM catalog_sales cs, date_dim d WHERE cs.cs_sold_date_sk = d.d_date_sk AND d.d_year = %d "
        .. "UNION ALL "
        .. "SELECT 'web', ws.ws_ext_sales_price "
        .. "FROM web_sales ws, date_dim d WHERE ws.ws_sold_date_sk = d.d_date_sk AND d.d_year = %d "
        .. ") t GROUP BY channel ORDER BY channel",
        y, y, y))
end

-- Q is global; transactions dispatch with the inline script "Q[n]()".
