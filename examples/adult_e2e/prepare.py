#!/usr/bin/env python3
"""Encode UCI Adult into token-safe categorical CSVs plus a PUBLIC domain spec.

Numeric columns are binned with FIXED public cut points (chosen from the documented
meaning of the attributes, not from the data), so binning spends no privacy budget.
Category domains are likewise hard-coded: on private data the domain must be public
schema knowledge, because a data-derived domain would leak which rare values exist.

Outputs (work/): train.csv (adult.data, what the fitter sees), holdout.csv (adult.test,
never seen by the fitter; used as TSTR test set and as the non-member set for the
membership-inference audit), domains.json.
"""
import csv, json, pathlib, sys

HERE = pathlib.Path(__file__).parent
DATA, WORK = HERE / "data", HERE / "work"

def tok(s):
    s = s.strip().rstrip(".")
    s = "Unknown" if s == "?" else s.replace("-", "_").replace("&", "and")
    # GraphQL enum tokens cannot start with a digit; the compiler would rewrite "10th" to
    # "V_10th", so we emit that form up front and real and synthetic tokens agree.
    return "V_" + s if s[0].isdigit() else s

def bins(v, cuts, names):
    for c, n in zip(cuts, names):
        if v <= c:
            return n
    return names[-1]

AGE = ([24, 34, 44, 54, 64], ["a17_24", "a25_34", "a35_44", "a45_54", "a55_64", "a65p"])
HRS = ([29, 39, 40, 49], ["h0_29", "h30_39", "h40", "h41_49", "h50p"])
GAIN = ([0, 4999], ["g0", "glow", "ghigh"])
LOSS = ([0, 1999], ["l0", "llow", "lhigh"])

DOMAINS = {
    "age": AGE[1],
    "workclass": ["Private", "Self_emp_not_inc", "Self_emp_inc", "Federal_gov", "Local_gov",
                  "State_gov", "Without_pay", "Never_worked", "Unknown"],
    "education": ["Bachelors", "Some_college", "V_11th", "HS_grad", "Prof_school", "Assoc_acdm",
                  "Assoc_voc", "V_9th", "V_7th_8th", "V_12th", "Masters", "V_1st_4th", "V_10th",
                  "Doctorate", "V_5th_6th", "Preschool"],
    "marital_status": ["Married_civ_spouse", "Divorced", "Never_married", "Separated", "Widowed",
                       "Married_spouse_absent", "Married_AF_spouse"],
    "occupation": ["Tech_support", "Craft_repair", "Other_service", "Sales", "Exec_managerial",
                   "Prof_specialty", "Handlers_cleaners", "Machine_op_inspct", "Adm_clerical",
                   "Farming_fishing", "Transport_moving", "Priv_house_serv", "Protective_serv",
                   "Armed_Forces", "Unknown"],
    "relationship": ["Wife", "Own_child", "Husband", "Not_in_family", "Other_relative", "Unmarried"],
    "race": ["White", "Asian_Pac_Islander", "Amer_Indian_Eskimo", "Other", "Black"],
    "sex": ["Female", "Male"],
    "capital_gain": GAIN[1],
    "capital_loss": LOSS[1],
    "hours_per_week": HRS[1],
    "native_country": ["United_States", "Other_country", "Unknown"],
    "income": ["inc_le50K", "inc_gt50K"],
}
COLS = list(DOMAINS)

def encode(r):
    age, wc, _fn, edu, _en, ms, occ, rel, race, sex, cg, cl, hrs, nc, inc = [x.strip() for x in r]
    nc = tok(nc)
    return [
        bins(int(age), *AGE), tok(wc), tok(edu), tok(ms), tok(occ), tok(rel), tok(race), tok(sex),
        bins(int(cg), *GAIN), bins(int(cl), *LOSS), bins(int(hrs), *HRS),
        "Unknown" if nc == "Unknown" else ("United_States" if nc == "United_States" else "Other_country"),
        "inc_gt50K" if inc.startswith(">50K") else "inc_le50K",
    ]

def load(path):
    out = []
    with open(path) as f:
        for r in csv.reader(f):
            if len(r) == 15:  # skips the adult.test banner line and blanks
                out.append(encode(r))
    return out

def main():
    WORK.mkdir(exist_ok=True)
    for src, dst in [("adult.data", "train.csv"), ("adult.test", "holdout.csv")]:
        rows = load(DATA / src)
        for row in rows:
            for c, v in zip(COLS, row):
                if v not in DOMAINS[c]:
                    sys.exit(f"value {v!r} outside public domain of {c}")
        with open(WORK / dst, "w", newline="") as f:
            w = csv.writer(f, lineterminator="\n")
            w.writerow(COLS)
            w.writerows(rows)
        print(dst, len(rows), "rows")
    (WORK / "domains.json").write_text(json.dumps(DOMAINS, indent=1))

if __name__ == "__main__":
    main()
