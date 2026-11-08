//! Native Rust Telecom NOC Alarm-Storm Generator and Validator (Layer 2 Causal Process Simulator).
//!
//! Preserves 100% schema, ontology-binding, causal ground-truth, and baseline scoring parity
//! with `src/sdg/packs/telecom/` and `src/sdg/validate.py` at native Rust speed.

use crate::ontology::{bind, sha256_file, Binding, BindingSpec, EnumBindingSpec};
use crate::rng::SeekableRng;
use anyhow::{anyhow, bail, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

pub const PACK_VERSION: &str = "0.2.0";
const MIN_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MIN_MS;

pub fn telecom_binding_spec() -> BindingSpec {
    let mut labels = BTreeMap::new();
    labels.insert("PoP".to_string(), vec![]);
    labels.insert(
        "Device".to_string(),
        vec![
            "noria:Resource".to_string(),
            "ietf-network:networks/network/node".to_string(),
        ],
    );
    labels.insert("Card".to_string(), vec!["noria:Resource".to_string()]);
    labels.insert(
        "Port".to_string(),
        vec![
            "noria:NetworkInterface".to_string(),
            "ietf-network-topology:networks/network/node/termination-point".to_string(),
        ],
    );
    labels.insert("Service".to_string(), vec!["noria:Service".to_string()]);
    labels.insert("Customer".to_string(), vec![]);
    labels.insert(
        "Alarm".to_string(),
        vec![
            "noria:EventRecord".to_string(),
            "ietf-alarms:alarms/alarm-list/alarm".to_string(),
            "tmf642:Alarm".to_string(),
        ],
    );
    labels.insert(
        "Incident".to_string(),
        vec!["noria:TroubleTicket".to_string()],
    );
    labels.insert(
        "Change".to_string(),
        vec!["noria:ChangeRequest".to_string()],
    );
    labels.insert("Runbook".to_string(), vec![]);
    labels.insert("Fault".to_string(), vec![]);

    let mut enums = BTreeMap::new();
    enums.insert(
        "perceived_severity".to_string(),
        EnumBindingSpec {
            vocabulary: "ietf-alarms".to_string(),
            owner: Some("ietf-alarms:typedef/severity".to_string()),
            scheme: None,
            exclude: vec![],
        },
    );
    enums.insert(
        "alarm_type".to_string(),
        EnumBindingSpec {
            vocabulary: "tmf642".to_string(),
            owner: Some("tmf642:AlarmType".to_string()),
            scheme: None,
            exclude: vec![],
        },
    );
    enums.insert(
        "probable_cause".to_string(),
        EnumBindingSpec {
            vocabulary: "noria-o".to_string(),
            owner: None,
            scheme: Some("ProbableCause".to_string()),
            exclude: vec![],
        },
    );

    BindingSpec {
        vocabularies: vec![
            "noria-o".to_string(),
            "ietf-alarms".to_string(),
            "ietf-network-topology".to_string(),
            "tmf642".to_string(),
        ],
        labels,
        enums,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TelecomConfig {
    pub pack: String,
    pub topology: String,
    pub start: String,
    pub slot_hours: i64,
    pub repair_median_ms: f64,
    pub overlap_window_ms: i64,
    pub network: NetworkConfig,
    pub alarms: AlarmsConfig,
    pub changes: ChangesConfig,
    pub tickets: TicketsConfig,
    pub scenarios: Vec<ScenarioConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub core_per_pop: usize,
    pub aggregation_per_pop: [i64; 2],
    pub access_per_pop: [i64; 2],
    pub ran_share: f64,
    pub dual_home_rate: f64,
    pub enterprise_vpns: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AlarmsConfig {
    pub emit_prob: BTreeMap<String, f64>,
    pub los_delay_ms: f64,
    pub poll_interval_ms: i64,
    pub service_delay_ms: f64,
    pub flap_prob: f64,
    pub duplicate_prob: f64,
    pub clock_skew_ms: f64,
    pub background_per_hour: f64,
    pub element_notify_ms: f64,
    pub neighbour_report_period_ms: i64,
    pub ran_neighbours: usize,
    pub cells_per_ran: i64,
    pub background_lead_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChangesConfig {
    pub fault_after_change_prob: f64,
    pub unrelated_per_slot: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TicketsConfig {
    pub misgroup_prob: f64,
    pub background_attach_prob: f64,
    pub duplicate_incident_prob: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScenarioConfig {
    pub split: String,
    pub kind: String,
    pub count: usize,
    pub fault_types: BTreeMap<String, u64>,
}

#[derive(Clone, Debug)]
struct PopInfo {
    node_id: i64,
    name: String,
    country: String,
    latitude: Option<f64>,
    longitude: Option<f64>,
}

#[derive(Clone, Debug)]
struct Topology {
    name: String,
    pops: Vec<PopInfo>,
    links: Vec<(i64, i64, String)>,
}

fn parse_graphml(path: &Path) -> Result<Topology> {
    let xml = fs::read_to_string(path)?;
    let mut reader = Reader::from_str(&xml);
    let mut keys: HashMap<String, String> = HashMap::new();
    let mut pops = Vec::new();
    let mut links = Vec::new();

    let mut in_node = false;
    let mut in_edge = false;
    let mut current_node_id = 0i64;
    let mut current_edge_src = 0i64;
    let mut current_edge_dst = 0i64;
    let mut current_data_key = String::new();
    let mut current_attrs: HashMap<String, String> = HashMap::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let tag = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                match tag.as_str() {
                    "key" => {
                        let mut id = String::new();
                        let mut attr_name = String::new();
                        for a in e.attributes().flatten() {
                            let k = String::from_utf8_lossy(a.key.as_ref());
                            let v = String::from_utf8_lossy(&a.value).to_string();
                            if k == "id" {
                                id = v;
                            } else if k == "attr.name" {
                                attr_name = v;
                            }
                        }
                        if !id.is_empty() && !attr_name.is_empty() {
                            keys.insert(id, attr_name);
                        }
                    }
                    "node" => {
                        in_node = true;
                        current_attrs.clear();
                        for a in e.attributes().flatten() {
                            if a.key.as_ref() == b"id" {
                                current_node_id =
                                    String::from_utf8_lossy(&a.value).parse().unwrap_or(0);
                            }
                        }
                    }
                    "edge" => {
                        in_edge = true;
                        current_attrs.clear();
                        for a in e.attributes().flatten() {
                            if a.key.as_ref() == b"source" {
                                current_edge_src =
                                    String::from_utf8_lossy(&a.value).parse().unwrap_or(0);
                            } else if a.key.as_ref() == b"target" {
                                current_edge_dst =
                                    String::from_utf8_lossy(&a.value).parse().unwrap_or(0);
                            }
                        }
                    }
                    "data" => {
                        current_data_key.clear();
                        for a in e.attributes().flatten() {
                            if a.key.as_ref() == b"key" {
                                let kid = String::from_utf8_lossy(&a.value).to_string();
                                if let Some(mapped) = keys.get(&kid) {
                                    current_data_key = mapped.clone();
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if (in_node || in_edge) && !current_data_key.is_empty() {
                    let txt = String::from_utf8_lossy(t.as_ref()).to_string();
                    current_attrs.insert(current_data_key.clone(), txt);
                }
            }
            Ok(Event::End(e)) => {
                let tag = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                match tag.as_str() {
                    "node" => {
                        in_node = false;
                        pops.push(PopInfo {
                            node_id: current_node_id,
                            name: current_attrs
                                .get("label")
                                .cloned()
                                .unwrap_or_else(|| current_node_id.to_string()),
                            country: current_attrs.get("Country").cloned().unwrap_or_default(),
                            latitude: current_attrs
                                .get("Latitude")
                                .and_then(|s| s.parse::<f64>().ok()),
                            longitude: current_attrs
                                .get("Longitude")
                                .and_then(|s| s.parse::<f64>().ok()),
                        });
                    }
                    "edge" => {
                        in_edge = false;
                        if current_edge_src != current_edge_dst {
                            links.push((
                                current_edge_src,
                                current_edge_dst,
                                current_attrs.get("LinkLabel").cloned().unwrap_or_default(),
                            ));
                        }
                    }
                    "data" => {
                        current_data_key.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => bail!("GraphML XML error: {}", e),
            _ => {}
        }
    }

    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("topology")
        .to_string();
    Ok(Topology { name, pops, links })
}

pub struct GraphStore {
    pub nodes: BTreeMap<String, Map<String, Value>>,
    pub edges: Vec<Map<String, Value>>,
}

impl GraphStore {
    pub fn new() -> Self {
        Self {
            nodes: BTreeMap::new(),
            edges: Vec::new(),
        }
    }

    pub fn add_node(&mut self, id: &str, label: &str, mut props: Map<String, Value>) {
        props.insert("id".to_string(), Value::String(id.to_string()));
        props.insert("label".to_string(), Value::String(label.to_string()));
        self.nodes.insert(id.to_string(), props);
    }

    pub fn add_edge(&mut self, src: &str, rel: &str, dst: &str, mut props: Map<String, Value>) {
        props.insert("src".to_string(), Value::String(src.to_string()));
        props.insert("rel".to_string(), Value::String(rel.to_string()));
        props.insert("dst".to_string(), Value::String(dst.to_string()));
        self.edges.push(props);
    }

    pub fn write(&self, out_dir: &Path) -> Result<BTreeMap<String, String>> {
        fs::create_dir_all(out_dir)?;
        let nodes_path = out_dir.join("nodes.jsonl");
        {
            let mut w = BufWriter::new(File::create(&nodes_path)?);
            for (_, node) in &self.nodes {
                serde_json::to_writer(&mut w, node)?;
                w.write_all(b"\n")?;
            }
            w.flush()?;
        }
        let edges_path = out_dir.join("edges.jsonl");
        {
            let mut w = BufWriter::new(File::create(&edges_path)?);
            for edge in &self.edges {
                serde_json::to_writer(&mut w, edge)?;
                w.write_all(b"\n")?;
            }
            w.flush()?;
        }
        let mut shas = BTreeMap::new();
        shas.insert("nodes.jsonl".to_string(), sha256_file(&nodes_path)?);
        shas.insert("edges.jsonl".to_string(), sha256_file(&edges_path)?);
        Ok(shas)
    }
}

pub fn specific_problem_for(cause: &str) -> String {
    match cause {
        "equipmentMalfunction" => "Board hardware fault".to_string(),
        "receiverFailure" => "Receive module failure".to_string(),
        "transmitterFailure" => "Transmit module failure".to_string(),
        "processorProblem" => "Main processing unit fault".to_string(),
        "softwareProgramAbnormallyTerminated" => "Process terminated unexpectedly".to_string(),
        "softwareError" => "Software exception raised".to_string(),
        "outOfMemory" => "Memory pool exhausted".to_string(),
        "powerProblem" => "Power input failure".to_string(),
        "lossOfSignal" => "R_LOS".to_string(),
        "lossOfFrame" => "R_LOF".to_string(),
        "degradedSignal" => "Bit error rate over threshold".to_string(),
        "remoteNodeTransmissionError" => "Remote defect indication".to_string(),
        "configurationOrCustomizationError" => "Configuration data inconsistent".to_string(),
        "communicationsSubsystemFailure" => "NE not reachable".to_string(),
        "communicationsProtocolError" => "Neighbour relation down".to_string(),
        "performanceDegraded" => "Service KPI below objective".to_string(),
        "bandwidthReduced" => "Throughput below committed rate".to_string(),
        "responseTimeExcessive" => "Round-trip delay over threshold".to_string(),
        "congestion" => "Interface congestion".to_string(),
        "thresholdCrossed" => "Performance counter over threshold".to_string(),
        "resourceAtOrNearingCapacity" => "Resource utilisation high".to_string(),
        "temperatureUnacceptable" => "Temperature over threshold".to_string(),
        "heatingOrVentilationOrCoolingSystemProblem" => "Fan unit failure".to_string(),
        other => other.to_string(),
    }
}

#[derive(Clone, Debug)]
struct ServiceDef {
    #[allow(dead_code)]
    id: String,
    kind: String,
    deps: Vec<(String, String)>,
}

struct Network {
    g: GraphStore,
    device_pop: BTreeMap<String, String>,
    device_role: BTreeMap<String, String>,
    device_cards: BTreeMap<String, Vec<String>>,
    card_ports: BTreeMap<String, Vec<String>>,
    port_card: BTreeMap<String, String>,
    card_device: BTreeMap<String, String>,
    peer: BTreeMap<String, String>,
    uplinks: BTreeMap<String, Vec<(String, String)>>,
    free: BTreeMap<String, VecDeque<String>>,
    pop_devices: BTreeMap<String, BTreeMap<String, Vec<String>>>,
    backbone: Vec<(String, String)>,
    services: Vec<ServiceDef>,
    pop_adj: BTreeMap<String, Vec<String>>,
}

impl Network {
    fn new() -> Self {
        Self {
            g: GraphStore::new(),
            device_pop: BTreeMap::new(),
            device_role: BTreeMap::new(),
            device_cards: BTreeMap::new(),
            card_ports: BTreeMap::new(),
            port_card: BTreeMap::new(),
            card_device: BTreeMap::new(),
            peer: BTreeMap::new(),
            uplinks: BTreeMap::new(),
            free: BTreeMap::new(),
            pop_devices: BTreeMap::new(),
            backbone: Vec::new(),
            services: Vec::new(),
            pop_adj: BTreeMap::new(),
        }
    }

    fn port_device(&self, port: &str) -> String {
        let card = &self.port_card[port];
        self.card_device[card].clone()
    }

    fn device_of(&self, r: &str) -> String {
        if self.device_cards.contains_key(r) {
            r.to_string()
        } else if let Some(d) = self.card_device.get(r) {
            d.clone()
        } else {
            self.port_device(r)
        }
    }
}

fn add_device(
    net: &mut Network,
    rng: &mut SeekableRng,
    pop_id: &str,
    role: &str,
    idx: usize,
) -> String {
    let vendors: &[(&str, &str)] = match role {
        "core" => &[("Altamira Networks", "AX-9600"), ("Corvex", "CR-8800")],
        "aggregation" => &[
            ("Altamira Networks", "AX-4400"),
            ("Brightline Systems", "BL-AG40"),
        ],
        "access" => &[("Brightline Systems", "BL-210"), ("Duneport", "DP-ACC12")],
        _ => &[("Duneport", "DP-gNB3"), ("Kestrel Radio", "KR-5G2")],
    };
    let (vendor, model) = *rng.choose(vendors);
    let pop_suffix = pop_id.split_once(':').map(|x| x.1).unwrap_or(pop_id);
    let role_pfx = &role[..3];
    let did = format!("dev:{}:{}{}", pop_suffix, role_pfx, idx);
    let name = did.split_once(':').map(|x| x.1).unwrap_or(&did).to_string();

    let mut props = Map::new();
    props.insert("name".to_string(), json!(name));
    props.insert("role".to_string(), json!(role));
    props.insert("vendor".to_string(), json!(vendor));
    props.insert("model".to_string(), json!(model));
    props.insert("synthetic".to_string(), json!(true));
    net.g.add_node(&did, "Device", props);
    net.g.add_edge(pop_id, "HOSTS", &did, Map::new());

    net.device_pop.insert(did.clone(), pop_id.to_string());
    net.device_role.insert(did.clone(), role.to_string());
    net.device_cards.insert(did.clone(), Vec::new());
    net.free.insert(did.clone(), VecDeque::new());
    net.uplinks.insert(did.clone(), Vec::new());

    let (lo, hi) = match role {
        "core" => (4, 6),
        "aggregation" => (2, 4),
        "access" => (1, 2),
        _ => (1, 1),
    };
    let ports_per_card: usize = match role {
        "core" | "aggregation" => 8,
        _ => 4,
    };
    let speed_gbps: i64 = match role {
        "core" | "aggregation" => 100,
        "access" => 10,
        _ => 25,
    };

    let n_cards = rng.range_i64(lo, hi) as usize;
    for c in 0..n_cards {
        let cid = format!("{}/c{}", did, c);
        let mut cprops = Map::new();
        cprops.insert("device_id".to_string(), json!(did));
        cprops.insert("slot".to_string(), json!(c));
        cprops.insert("kind".to_string(), json!("line"));
        cprops.insert("synthetic".to_string(), json!(true));
        net.g.add_node(&cid, "Card", cprops);
        net.g.add_edge(&did, "HAS_CARD", &cid, Map::new());

        net.device_cards.get_mut(&did).unwrap().push(cid.clone());
        net.card_device.insert(cid.clone(), did.clone());
        net.card_ports.insert(cid.clone(), Vec::new());

        for p in 0..ports_per_card {
            let pid = format!("{}/p{}", cid, p);
            let mut pprops = Map::new();
            pprops.insert("card_id".to_string(), json!(cid));
            pprops.insert("index".to_string(), json!(p));
            pprops.insert("speed_gbps".to_string(), json!(speed_gbps));
            pprops.insert("synthetic".to_string(), json!(true));
            net.g.add_node(&pid, "Port", pprops);
            net.g.add_edge(&cid, "HAS_PORT", &pid, Map::new());
            net.card_ports.get_mut(&cid).unwrap().push(pid.clone());
            net.port_card.insert(pid, cid.clone());
        }
    }

    let cards = net.device_cards[&did].clone();
    let mut order = Vec::new();
    for p in 0..ports_per_card {
        for c in &cards {
            order.push(net.card_ports[c][p].clone());
        }
    }
    net.free.get_mut(&did).unwrap().extend(order);
    did
}

fn connect(
    net: &mut Network,
    a: &str,
    b: &str,
    at: i64,
    link_id: &str,
    backbone: bool,
) -> Result<(String, String)> {
    let pa = net
        .free
        .get_mut(a)
        .and_then(|q| q.pop_front())
        .ok_or_else(|| anyhow!("out of ports on {}", a))?;
    let pb = net
        .free
        .get_mut(b)
        .and_then(|q| q.pop_front())
        .ok_or_else(|| anyhow!("out of ports on {}", b))?;
    let mut eprops = Map::new();
    eprops.insert("link_id".to_string(), json!(link_id));
    eprops.insert("backbone".to_string(), json!(backbone));
    eprops.insert("at".to_string(), json!(at));
    net.g.add_edge(&pa, "CONNECTS", &pb, eprops);
    net.peer.insert(pa.clone(), pb.clone());
    net.peer.insert(pb.clone(), pa.clone());
    Ok((pa, pb))
}

fn bfs_path(
    adj: &BTreeMap<i64, Vec<i64>>,
    a: i64,
    b: i64,
    banned: &BTreeSet<i64>,
) -> Option<Vec<i64>> {
    let mut prev: BTreeMap<i64, Option<i64>> = BTreeMap::new();
    let mut q = VecDeque::new();
    prev.insert(a, None);
    q.push_back(a);
    while let Some(u) = q.pop_front() {
        if u == b {
            let mut out = vec![u];
            while let Some(Some(p)) = prev.get(out.last().unwrap()) {
                out.push(*p);
            }
            out.reverse();
            return Some(out);
        }
        if let Some(nbrs) = adj.get(&u) {
            let mut sorted_nbrs = nbrs.clone();
            sorted_nbrs.sort_unstable();
            for v in sorted_nbrs {
                if !prev.contains_key(&v) && (!banned.contains(&v) || v == b) {
                    prev.insert(v, Some(u));
                    q.push_back(v);
                }
            }
        }
    }
    None
}

fn build_network(
    topo: &Topology,
    cfg: &NetworkConfig,
    rng: &mut SeekableRng,
    t0: i64,
) -> Result<Network> {
    let mut net = Network::new();
    let n = &topo.name;

    for p in &topo.pops {
        let pid = format!("pop:{}:{}", n, p.node_id);
        let mut pprops = Map::new();
        pprops.insert("network".to_string(), json!(n));
        pprops.insert("synthetic".to_string(), json!(false));
        pprops.insert("node_id".to_string(), json!(p.node_id));
        pprops.insert("name".to_string(), json!(p.name));
        pprops.insert("country".to_string(), json!(p.country));
        pprops.insert("latitude".to_string(), json!(p.latitude));
        pprops.insert("longitude".to_string(), json!(p.longitude));
        net.g.add_node(&pid, "PoP", pprops);

        let mut roles: BTreeMap<String, Vec<String>> = BTreeMap::new();
        roles.insert("core".to_string(), Vec::new());
        roles.insert("aggregation".to_string(), Vec::new());
        roles.insert("access".to_string(), Vec::new());
        roles.insert("ran".to_string(), Vec::new());

        for i in 0..cfg.core_per_pop {
            let d = add_device(&mut net, rng, &pid, "core", i);
            roles.get_mut("core").unwrap().push(d);
        }
        let n_agg = rng.range_i64(cfg.aggregation_per_pop[0], cfg.aggregation_per_pop[1]) as usize;
        for i in 0..n_agg {
            let agg = add_device(&mut net, rng, &pid, "aggregation", i);
            roles.get_mut("aggregation").unwrap().push(agg.clone());
            for core in &roles["core"] {
                let (pa, _) = connect(&mut net, &agg, core, t0, &format!("{}~{}", agg, core), false)?;
                net.uplinks.get_mut(&agg).unwrap().push((pa, core.clone()));
            }
        }
        let n_acc = rng.range_i64(cfg.access_per_pop[0], cfg.access_per_pop[1]) as usize;
        for i in 0..n_acc {
            let role = if rng.next_f64() < cfg.ran_share {
                "ran"
            } else {
                "access"
            };
            let dev = add_device(&mut net, rng, &pid, role, i);
            roles.get_mut(role).unwrap().push(dev.clone());
            let aggs = &roles["aggregation"];
            let first = rng.choose(aggs).clone();
            let mut homes = vec![first.clone()];
            if aggs.len() > 1 && rng.next_f64() < cfg.dual_home_rate {
                if let Some(other) = aggs.iter().find(|x| **x != first) {
                    homes.push(other.clone());
                }
            }
            for agg in homes {
                let (pa, _) = connect(&mut net, &dev, &agg, t0, &format!("{}~{}", dev, agg), false)?;
                net.uplinks.get_mut(&dev).unwrap().push((pa, agg));
            }
        }
        net.pop_devices.insert(pid, roles);
    }

    for (k, (a, b, label)) in topo.links.iter().enumerate() {
        let pa_id = format!("pop:{}:{}", n, a);
        let pb_id = format!("pop:{}:{}", n, b);
        net.pop_adj
            .entry(pa_id.clone())
            .or_default()
            .push(pb_id.clone());
        net.pop_adj
            .entry(pb_id.clone())
            .or_default()
            .push(pa_id.clone());
        let mut lprops = Map::new();
        lprops.insert("link_id".to_string(), json!(k));
        lprops.insert("link_label".to_string(), json!(label));
        net.g.add_edge(&pa_id, "LINK", &pb_id, lprops);

        let ca = net.pop_devices[&pa_id]["core"][k % cfg.core_per_pop].clone();
        let cb = net.pop_devices[&pb_id]["core"][k % cfg.core_per_pop].clone();
        let ports = connect(
            &mut net,
            &ca,
            &cb,
            t0,
            &format!("bb:{}:{}", n, k),
            true,
        )?;
        net.backbone.push(ports);
    }

    let pop_keys: Vec<String> = net.pop_devices.keys().cloned().collect();
    for pid in pop_keys {
        let cores = net.pop_devices[&pid]["core"].clone();
        for w in cores.windows(2) {
            connect(&mut net, &w[0], &w[1], t0, &format!("{}~{}", w[0], w[1]), false)?;
        }
    }

    // Build services
    let mut k = 0usize;
    let pop_keys: Vec<String> = net.pop_devices.keys().cloned().collect();
    for pid in pop_keys {
        let mut edge_devs = net.pop_devices[&pid]["ran"].clone();
        edge_devs.extend(net.pop_devices[&pid]["access"].clone());
        for dev in edge_devs {
            let kind = if net.device_role[&dev] == "ran" {
                "mobile-data"
            } else {
                "broadband"
            };
            let sla = if kind == "broadband" {
                "bronze"
            } else {
                "silver"
            };
            let mut deps = vec![(dev.clone(), "primary".to_string())];
            for (i, (_, up)) in net.uplinks[&dev].iter().enumerate() {
                deps.push((
                    up.clone(),
                    if i == 0 {
                        "primary".to_string()
                    } else {
                        "backup".to_string()
                    },
                ));
            }
            let sid = format!("svc:{}:{}", n, k);
            let mut sprops = Map::new();
            sprops.insert("kind".to_string(), json!(kind));
            sprops.insert("sla".to_string(), json!(sla));
            sprops.insert("synthetic".to_string(), json!(true));
            net.g.add_node(&sid, "Service", sprops);
            for (dst, r) in &deps {
                let mut dprops = Map::new();
                dprops.insert("role".to_string(), json!(r));
                dprops.insert("at".to_string(), json!(t0));
                net.g.add_edge(&sid, "DEPENDS_ON", dst, dprops);
            }
            let cid = format!("cust:{}:{}", n, k);
            let mut cprops = Map::new();
            cprops.insert("segment".to_string(), json!("consumer"));
            cprops.insert("synthetic".to_string(), json!(true));
            net.g.add_node(&cid, "Customer", cprops);
            net.g.add_edge(&cid, "SUBSCRIBES", &sid, Map::new());
            net.services.push(ServiceDef {
                id: sid,
                kind: kind.to_string(),
                deps,
            });
            k += 1;
        }
    }

    let mut adj: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for p in &topo.pops {
        adj.insert(p.node_id, Vec::new());
    }
    for (a, b, _) in &topo.links {
        adj.entry(*a).or_default().push(*b);
        adj.entry(*b).or_default().push(*a);
    }
    let pop_ids: Vec<i64> = topo.pops.iter().map(|p| p.node_id).collect();
    if pop_ids.len() >= 2 {
        for _ in 0..cfg.enterprise_vpns {
            let i1 = rng.integers(pop_ids.len() as u64) as usize;
            let mut i2 = rng.integers((pop_ids.len() - 1) as u64) as usize;
            if i2 >= i1 {
                i2 += 1;
            }
            let (a, b) = (pop_ids[i1], pop_ids[i2]);
            let Some(primary) = bfs_path(&adj, a, b, &BTreeSet::new()) else {
                continue;
            };
            let backup = if primary.len() > 2 {
                let banned: BTreeSet<i64> = primary[1..primary.len() - 1].iter().copied().collect();
                bfs_path(&adj, a, b, &banned)
            } else {
                None
            };
            let mut deps: Vec<(String, String)> = Vec::new();
            for (role, path) in [
                ("primary", primary.as_slice()),
                ("backup", backup.as_deref().unwrap_or(&[])),
            ] {
                for &pop in path {
                    let cores = &net.pop_devices[&format!("pop:{}:{}", n, pop)]["core"];
                    let core = if role == "primary" {
                        cores[0].clone()
                    } else {
                        cores.last().unwrap().clone()
                    };
                    if !deps.iter().any(|(d, r)| d == &core && r == "primary") {
                        deps.push((core, role.to_string()));
                    }
                }
            }
            for pop in [a, b] {
                let pd = &net.pop_devices[&format!("pop:{}:{}", n, pop)];
                let pool = if !pd["access"].is_empty() {
                    &pd["access"]
                } else {
                    &pd["aggregation"]
                };
                let chosen = rng.choose(pool).clone();
                deps.insert(0, (chosen, "primary".to_string()));
            }
            let sid = format!("svc:{}:{}", n, k);
            let mut sprops = Map::new();
            sprops.insert("kind".to_string(), json!("enterprise-vpn"));
            sprops.insert("sla".to_string(), json!("gold"));
            sprops.insert("synthetic".to_string(), json!(true));
            net.g.add_node(&sid, "Service", sprops);
            for (dst, r) in &deps {
                let mut dprops = Map::new();
                dprops.insert("role".to_string(), json!(r));
                dprops.insert("at".to_string(), json!(t0));
                net.g.add_edge(&sid, "DEPENDS_ON", dst, dprops);
            }
            let cid = format!("cust:{}:{}", n, k);
            let mut cprops = Map::new();
            cprops.insert("segment".to_string(), json!("enterprise"));
            cprops.insert("synthetic".to_string(), json!(true));
            net.g.add_node(&cid, "Customer", cprops);
            net.g.add_edge(&cid, "SUBSCRIBES", &sid, Map::new());
            net.services.push(ServiceDef {
                id: sid,
                kind: "enterprise-vpn".to_string(),
                deps,
            });
            k += 1;
        }
    }

    Ok(net)
}

#[derive(Clone, Debug)]
struct Fault {
    id: String,
    fault_type: String,
    resources: Vec<String>,
    onset: i64,
    repair: i64,
    scenario: String,
    change: Option<String>,
    ports: Vec<String>,
}

struct Outcome {
    #[allow(dead_code)]
    down_ports: BTreeSet<String>,
    down_devices: BTreeSet<String>,
    los_ports: BTreeSet<String>,
    isolated: BTreeSet<String>,
    services_out: Vec<(ServiceDef, Vec<String>)>,
}

fn compute_effect(net: &Network, faults: &[Fault], noc_pop: &str) -> Outcome {
    let mut down_ports = BTreeSet::new();
    let mut down_devices = BTreeSet::new();
    for f in faults {
        for r in &f.resources {
            if r.starts_with("pop:") {
                for devs in net.pop_devices[r].values() {
                    down_devices.extend(devs.iter().cloned());
                }
            } else if net.device_cards.contains_key(r) {
                if f.fault_type == "config_change" {
                    down_ports.extend(f.ports.iter().cloned());
                } else {
                    down_devices.insert(r.clone());
                }
            } else if let Some(ports) = net.card_ports.get(r) {
                down_ports.extend(ports.iter().cloned());
            } else {
                down_ports.insert(r.clone());
            }
        }
    }
    for d in &down_devices {
        for c in &net.device_cards[d] {
            down_ports.extend(net.card_ports[c].iter().cloned());
        }
    }
    let los_ports: BTreeSet<String> = down_ports
        .iter()
        .filter_map(|p| net.peer.get(p).cloned())
        .filter(|peer| !down_ports.contains(peer))
        .collect();

    let start: Vec<String> = net.pop_devices[noc_pop]["core"]
        .iter()
        .filter(|d| !down_devices.contains(*d))
        .cloned()
        .collect();
    let mut seen: HashSet<String> = start.iter().cloned().collect();
    let mut q: VecDeque<String> = start.into_iter().collect();

    let mut links: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (p, peer) in &net.peer {
        links
            .entry(net.port_device(p))
            .or_default()
            .push((p.clone(), peer.clone()));
    }
    while let Some(u) = q.pop_front() {
        if let Some(adj) = links.get(&u) {
            for (p, peer) in adj {
                let v = net.port_device(peer);
                if seen.contains(&v)
                    || down_devices.contains(&v)
                    || down_ports.contains(p)
                    || down_ports.contains(peer)
                {
                    continue;
                }
                seen.insert(v.clone());
                q.push_back(v);
            }
        }
    }
    let isolated: BTreeSet<String> = net
        .device_role
        .keys()
        .filter(|d| !seen.contains(*d) && !down_devices.contains(*d))
        .cloned()
        .collect();

    let mut services_out = Vec::new();
    for s in &net.services {
        let take_n = if s.kind == "enterprise-vpn" { 2 } else { 1 };
        let ends: Vec<String> = s.deps.iter().take(take_n).map(|(d, _)| d.clone()).collect();
        let any_dead = ends
            .iter()
            .any(|d| down_devices.contains(d) || isolated.contains(d));
        if any_dead {
            let live_ends: Vec<String> = ends
                .into_iter()
                .filter(|d| !down_devices.contains(d) && !isolated.contains(d))
                .collect();
            services_out.push((s.clone(), live_ends));
        }
    }

    Outcome {
        down_ports,
        down_devices,
        los_ports,
        isolated,
        services_out,
    }
}

fn pick_fault(
    net: &Network,
    rng: &mut SeekableRng,
    fault_type: &str,
    onset: i64,
    repair_median_ms: f64,
    fid: &str,
    scenario: &str,
    pop_override: Option<&str>,
) -> Fault {
    let pops: Vec<String> = net.pop_devices.keys().cloned().collect();
    let pop = pop_override
        .map(String::from)
        .unwrap_or_else(|| rng.choose(&pops).clone());
    let roles = &net.pop_devices[&pop];

    let res = match fault_type {
        "card_failure" => {
            let mut pool = roles["aggregation"].clone();
            pool.extend(roles["core"].iter().cloned());
            let dev = rng.choose(&pool);
            vec![rng.choose(&net.device_cards[dev]).clone()]
        }
        "device_failure" | "software_crash" | "config_change" => {
            let mut pool = roles["aggregation"].clone();
            pool.extend(roles["core"].iter().cloned());
            pool.extend(roles["access"].iter().cloned());
            vec![rng.choose(&pool).clone()]
        }
        "port_failure" => {
            let dev = rng.choose(&roles["aggregation"]);
            let cabled: Vec<String> = net.device_cards[dev]
                .iter()
                .flat_map(|c| net.card_ports[c].iter())
                .filter(|p| net.peer.contains_key(*p))
                .cloned()
                .collect();
            vec![rng.choose(&cabled).clone()]
        }
        "fibre_cut" => {
            let mine: Vec<(String, String)> = net
                .backbone
                .iter()
                .filter(|(p0, _)| net.device_pop[&net.port_device(p0)] == pop)
                .cloned()
                .collect();
            let pair = if !mine.is_empty() {
                rng.choose(&mine)
            } else {
                rng.choose(&net.backbone)
            };
            vec![pair.0.clone(), pair.1.clone()]
        }
        _ => vec![pop],
    };

    let mut ports = Vec::new();
    if fault_type == "config_change" {
        let cabled: Vec<String> = net.device_cards[&res[0]]
            .iter()
            .flat_map(|c| net.card_ports[c].iter())
            .filter(|p| net.peer.contains_key(*p))
            .cloned()
            .collect();
        for p in &cabled {
            if rng.next_f64() < 0.5 {
                ports.push(p.clone());
            }
        }
        if ports.is_empty() && !cabled.is_empty() {
            ports.push(cabled[0].clone());
        }
    }

    let repair = onset + (rng.lognormal_median(repair_median_ms, 0.6) as i64);
    Fault {
        id: fid.to_string(),
        fault_type: fault_type.to_string(),
        resources: res,
        onset,
        repair,
        scenario: scenario.to_string(),
        change: None,
        ports,
    }
}

#[derive(Clone, Debug)]
struct AlarmRecord {
    id: String,
    alarm_type: String,
    perceived_severity: String,
    probable_cause: String,
    specific_problem: String,
    service_affecting: bool,
    scenario: String,
    resource: String,
    mechanism: String,
    raised_at: i64,
    cleared_at: Option<i64>,
}

fn causes_for_mechanism(mech: &str) -> &'static [&'static str] {
    match mech {
        "card_failure" => &["equipmentMalfunction", "receiverFailure", "transmitterFailure"],
        "device_failure" => &["equipmentMalfunction", "processorProblem"],
        "software_crash" => &[
            "softwareProgramAbnormallyTerminated",
            "softwareError",
            "outOfMemory",
        ],
        "power_loss" => &["powerProblem"],
        "port_failure" => &["lossOfSignal", "transmitterFailure"],
        "fibre_cut" => &["lossOfSignal"],
        "config_change" => &["configurationOrCustomizationError"],
        "loss-of-signal" => &[
            "lossOfSignal",
            "lossOfFrame",
            "degradedSignal",
            "remoteNodeTransmissionError",
        ],
        "isolation" => &["communicationsSubsystemFailure"],
        "service" => &["performanceDegraded", "bandwidthReduced", "responseTimeExcessive"],
        "congestion" => &["congestion", "thresholdCrossed", "resourceAtOrNearingCapacity"],
        "neighbour" => &["communicationsProtocolError", "remoteNodeTransmissionError"],
        _ => &[
            "thresholdCrossed",
            "temperatureUnacceptable",
            "performanceDegraded",
            "heatingOrVentilationOrCoolingSystemProblem",
            "bandwidthReduced",
        ],
    }
}

fn alarm_type_for(kind: &str) -> &'static str {
    match kind {
        "card_failure" | "device_failure" => "equipmentAlarm",
        "software_crash" | "config_change" => "processingErrorAlarm",
        "power_loss" => "environmentalAlarm",
        "port_failure" | "fibre_cut" | "loss-of-signal" | "isolation" | "neighbour" => {
            "communicationsAlarm"
        }
        _ => "qualityOfServiceAlarm",
    }
}

pub fn generate_telecom(
    cfg: &TelecomConfig,
    seed: u64,
    ontologies_build: &Path,
    out_dir: &Path,
    config_path: Option<&Path>,
) -> Result<Value> {
    let binding = bind(ontologies_build, &telecom_binding_spec())?;
    let topo_path = Path::new(&cfg.topology);
    let topo = parse_graphml(topo_path)?;
    let t0 = 1_767_571_200_000i64; // 2026-01-05T00:00:00Z

    let mut topo_rng = SeekableRng::for_stage(seed, "topology");
    let mut net = build_network(&topo, &cfg.network, &mut topo_rng, t0)?;

    let mut skew_rng = SeekableRng::for_stage(seed, "skew");
    let mut skew: BTreeMap<String, i64> = BTreeMap::new();
    for d in net.device_role.keys() {
        skew.insert(d.clone(), skew_rng.normal(0.0, cfg.alarms.clock_skew_ms) as i64);
    }

    let mut degree: BTreeMap<String, usize> = BTreeMap::new();
    for (a, b, _) in &topo.links {
        for x in [a, b] {
            let pid = format!("pop:{}:{}", topo.name, x);
            *degree.entry(pid).or_insert(0) += 1;
        }
    }
    let noc_pop = degree
        .iter()
        .max_by_key(|(_, &deg)| deg)
        .map(|(k, _)| k.clone())
        .unwrap_or_else(|| net.pop_devices.keys().next().unwrap().clone());

    let mut arng = SeekableRng::for_stage(seed, "alarms");
    let mut frng = SeekableRng::for_stage(seed, "faults");
    let mut trng = SeekableRng::for_stage(seed, "tickets");
    let mut crng = SeekableRng::for_stage(seed, "changes");
    let mut nrng = SeekableRng::for_stage(seed, "notify");

    let bound_causes: HashSet<String> = binding.values("probable_cause").into_iter().collect();
    let slot = cfg.slot_hours * HOUR_MS;

    let mut alarms: Vec<AlarmRecord> = Vec::new();
    let mut caused: Vec<(String, String, String, i64)> = Vec::new();
    let mut alarm_k = 0usize;
    let mut truth: Vec<Value> = Vec::new();
    let mut all_incidents: Vec<Map<String, Value>> = Vec::new();
    let mut changes: Vec<Map<String, Value>> = Vec::new();

    let raise_alarm = |resource: &str,
                           mechanism: &str,
                           at_base: i64,
                           cleared: Option<i64>,
                           scenario: &str,
                           fault: Option<&Fault>,
                           hops: i64,
                           severity: &str,
                           service_affecting: bool,
                           arng: &mut SeekableRng,
                           alarms: &mut Vec<AlarmRecord>,
                           caused: &mut Vec<(String, String, String, i64)>,
                           alarm_k: &mut usize| {
        if fault.is_some() {
            let prob = cfg.alarms.emit_prob.get(mechanism).copied().unwrap_or(1.0);
            if arng.next_f64() > prob {
                return;
            }
        }
        let dev = net.device_of(resource);
        let at = at_base + skew.get(&dev).copied().unwrap_or(0);
        let kind = if mechanism == "root" {
            fault.map(|f| f.fault_type.as_str()).unwrap_or(mechanism)
        } else {
            mechanism
        };
        let candidates: Vec<&str> = causes_for_mechanism(kind)
            .iter()
            .copied()
            .filter(|c| bound_causes.contains(*c))
            .collect();
        let cause = (*arng.choose(&candidates)).to_string();
        let spec_prob = specific_problem_for(&cause);
        let atype = alarm_type_for(kind).to_string();

        let mut times = vec![(at, cleared)];
        if (mechanism == "loss-of-signal" || mechanism == "background")
            && arng.next_f64() < cfg.alarms.flap_prob
        {
            let mut t = at;
            let mut flaps = Vec::new();
            let n_flaps = arng.range_i64(2, 4);
            for _ in 0..n_flaps {
                let up = t + arng.range_i64(5_000, 90_000);
                flaps.push((t, Some(up)));
                t = up + arng.range_i64(5_000, 60_000);
            }
            flaps.push((t, cleared));
            times = flaps;
        }
        if arng.next_f64() < cfg.alarms.duplicate_prob {
            times.push((at + arng.range_i64(500, 15_000), cleared));
        }

        for (raised, clr) in times {
            if let Some(c) = clr {
                if raised > c {
                    continue;
                }
            }
            let aid = format!("alm:{}:{}", scenario, *alarm_k);
            *alarm_k += 1;
            alarms.push(AlarmRecord {
                id: aid.clone(),
                alarm_type: atype.clone(),
                perceived_severity: severity.to_string(),
                probable_cause: cause.clone(),
                specific_problem: spec_prob.clone(),
                service_affecting,
                scenario: scenario.to_string(),
                resource: resource.to_string(),
                mechanism: mechanism.to_string(),
                raised_at: raised,
                cleared_at: clr,
            });
            if let Some(f) = fault {
                caused.push((f.id.clone(), aid, mechanism.to_string(), hops));
            }
        }
    };

    let mut i = 0usize;
    for sc in &cfg.scenarios {
        let total_weight: u64 = sc.fault_types.values().sum();
        let weighted_types: Vec<(&String, u64)> =
            sc.fault_types.iter().map(|(k, &v)| (k, v)).collect();

        let choose_fault_type = |rng: &mut SeekableRng| -> String {
            let mut draw = rng.integers(total_weight);
            for &(ft, w) in &weighted_types {
                if draw < w {
                    return (*ft).clone();
                }
                draw -= w;
            }
            weighted_types[0].0.clone()
        };

        for _ in 0..sc.count {
            let scenario = format!("{}-{}-{:04}", sc.split, sc.kind, i);
            let start = t0 + (i as i64) * slot;
            let onset = start + frng.range_i64(slot / 5, slot / 2);
            let ft = choose_fault_type(&mut frng);
            let mut fs = vec![pick_fault(
                &net,
                &mut frng,
                &ft,
                onset,
                cfg.repair_median_ms,
                &format!("flt:{}:0", scenario),
                &scenario,
                None,
            )];
            if sc.kind == "overlap" {
                let first_res = &fs[0].resources[0];
                let pop = if first_res.starts_with("pop:") {
                    first_res.clone()
                } else {
                    net.device_pop[&net.device_of(first_res)].clone()
                };
                let ft2 = choose_fault_type(&mut frng);
                let onset2 = onset + frng.range_i64(0, cfg.overlap_window_ms);
                fs.push(pick_fault(
                    &net,
                    &mut frng,
                    &ft2,
                    onset2,
                    cfg.repair_median_ms,
                    &format!("flt:{}:1", scenario),
                    &scenario,
                    Some(&pop),
                ));
            }

            for f in &mut fs {
                if f.fault_type == "config_change"
                    || frng.next_f64() < cfg.changes.fault_after_change_prob
                {
                    if !f.resources[0].starts_with("pop:") {
                        let target = f.resources[0].clone();
                        let suffix = f.id.rsplit(':').next().unwrap_or("0");
                        let cid = format!("chg:{}:{}", scenario, suffix);
                        let lead = crng.range_i64(MIN_MS, 20 * MIN_MS);
                        let mut cnode = Map::new();
                        cnode.insert("id".to_string(), json!(cid));
                        cnode.insert(
                            "kind".to_string(),
                            json!(if f.fault_type != "config_change" {
                                "software-upgrade"
                            } else {
                                "configuration"
                            }),
                        );
                        cnode.insert(
                            "planned_start".to_string(),
                            json!(f.onset - lead - 10 * MIN_MS),
                        );
                        cnode.insert("actual_start".to_string(), json!(f.onset - lead));
                        cnode.insert("outcome".to_string(), json!("failed"));
                        cnode.insert("target".to_string(), json!(target));
                        changes.push(cnode);
                        f.change = Some(cid);
                    }
                }
            }

            let window = (start, start + slot);
            let n0 = alarms.len();
            let out = compute_effect(&net, &fs, &noc_pop);
            let end = window.1;

            for f in &fs {
                let clr = {
                    let c = f.repair + arng.range_i64(5_000, 120_000);
                    if c <= end {
                        Some(c)
                    } else {
                        None
                    }
                };
                for r in &f.resources {
                    if !r.starts_with("pop:")
                        && (!net.device_cards.contains_key(r)
                            || f.fault_type == "software_crash"
                            || f.fault_type == "config_change")
                    {
                        let delay = arng.lognormal_median(1_500.0, 0.6) as i64;
                        raise_alarm(
                            r,
                            "root",
                            f.onset + delay,
                            clr,
                            &scenario,
                            Some(f),
                            0,
                            "critical",
                            false,
                            &mut arng,
                            &mut alarms,
                            &mut caused,
                            &mut alarm_k,
                        );
                    }
                }
            }

            for p in &out.los_ports {
                let f = &fs[0];
                let clr = {
                    let c = f.repair + arng.range_i64(5_000, 120_000);
                    if c <= end {
                        Some(c)
                    } else {
                        None
                    }
                };
                let delay = arng.lognormal_median(cfg.alarms.los_delay_ms, 0.5) as i64;
                raise_alarm(
                    p,
                    "loss-of-signal",
                    f.onset + delay,
                    clr,
                    &scenario,
                    Some(f),
                    1,
                    "major",
                    false,
                    &mut arng,
                    &mut alarms,
                    &mut caused,
                    &mut alarm_k,
                );
            }

            for d in &out.isolated {
                let f = &fs[0];
                let clr = {
                    let c = f.repair + arng.range_i64(5_000, 120_000);
                    if c <= end {
                        Some(c)
                    } else {
                        None
                    }
                };
                let poll = arng.range_i64(0, cfg.alarms.poll_interval_ms);
                let delay = arng.lognormal_median(5_000.0, 0.5) as i64;
                raise_alarm(
                    d,
                    "isolation",
                    f.onset + poll + delay,
                    clr,
                    &scenario,
                    Some(f),
                    2,
                    "critical",
                    true,
                    &mut arng,
                    &mut alarms,
                    &mut caused,
                    &mut alarm_k,
                );
            }

            for (s, live_ends) in &out.services_out {
                let f = &fs[0];
                let clr = {
                    let c = f.repair + arng.range_i64(5_000, 120_000);
                    if c <= end {
                        Some(c)
                    } else {
                        None
                    }
                };
                let mut ends = live_ends.clone();
                if s.kind == "mobile-data" {
                    if let Some((d, _)) = s
                        .deps
                        .iter()
                        .skip(1)
                        .find(|(d, _)| !out.down_devices.contains(d) && !out.isolated.contains(d))
                    {
                        ends.push(d.clone());
                    }
                }
                for d in ends {
                    let delay = arng.lognormal_median(cfg.alarms.service_delay_ms, 0.6) as i64;
                    raise_alarm(
                        &d,
                        "service",
                        f.onset + delay,
                        clr,
                        &scenario,
                        Some(f),
                        3,
                        "major",
                        true,
                        &mut arng,
                        &mut alarms,
                        &mut caused,
                        &mut alarm_k,
                    );
                }
            }

            let mut dead_ran: Vec<String> = out
                .down_devices
                .union(&out.isolated)
                .filter(|d| net.device_role.get(*d).map(|r| r.as_str()) == Some("ran"))
                .cloned()
                .collect();
            dead_ran.sort();
            for d in dead_ran {
                let f = &fs[0];
                let clr = {
                    let c = f.repair + arng.range_i64(5_000, 120_000);
                    if c <= end {
                        Some(c)
                    } else {
                        None
                    }
                };
                let pop = &net.device_pop[&d];
                let mut check_pops = vec![pop.clone()];
                if let Some(adj_pops) = net.pop_adj.get(pop) {
                    let mut s_adj = adj_pops.clone();
                    s_adj.sort();
                    check_pops.extend(s_adj);
                }
                let mut near = Vec::new();
                for p in check_pops {
                    for x in &net.pop_devices[&p]["ran"] {
                        if x != &d
                            && !out.down_devices.contains(x)
                            && !out.isolated.contains(x)
                        {
                            near.push(x.clone());
                        }
                    }
                }
                for x in near.into_iter().take(cfg.alarms.ran_neighbours) {
                    let period = cfg.alarms.neighbour_report_period_ms;
                    let report = if period > 0 {
                        arng.range_i64(0, period)
                    } else {
                        0
                    };
                    let n_cells = arng.range_i64(1, cfg.alarms.cells_per_ran);
                    for _ in 0..n_cells {
                        let delay = arng.lognormal_median(20_000.0, 0.6) as i64;
                        raise_alarm(
                            &x,
                            "neighbour",
                            f.onset + report + delay,
                            clr,
                            &scenario,
                            Some(f),
                            2,
                            "minor",
                            false,
                            &mut arng,
                            &mut alarms,
                            &mut caused,
                            &mut alarm_k,
                        );
                    }
                }
            }

            // Background alarms
            let min_onset = fs.iter().map(|f| f.onset).min().unwrap_or(start);
            let bg_win = (min_onset - cfg.alarms.background_lead_ms, end);
            let hours = ((bg_win.1 - bg_win.0) as f64) / (HOUR_MS as f64);
            let n_bg = arng.poisson(cfg.alarms.background_per_hour * hours);
            let all_ports: Vec<String> = net.peer.keys().cloned().collect();
            let all_devs: Vec<String> = net.device_role.keys().cloned().collect();
            for _ in 0..n_bg {
                let r = if arng.next_f64() < 0.4 {
                    arng.choose(&all_devs).clone()
                } else {
                    arng.choose(&all_ports).clone()
                };
                let at = arng.range_i64(bg_win.0, bg_win.1);
                let dur = arng.lognormal_median((10 * MIN_MS) as f64, 1.0) as i64;
                let clr = if at + dur <= bg_win.1 {
                    Some(at + dur)
                } else {
                    None
                };
                let sev = if arng.next_f64() < 0.7 {
                    "warning"
                } else {
                    "minor"
                };
                raise_alarm(
                    &r,
                    "background",
                    at,
                    clr,
                    &scenario,
                    None,
                    -1,
                    sev,
                    false,
                    &mut arng,
                    &mut alarms,
                    &mut caused,
                    &mut alarm_k,
                );
            }

            // Serialise per-element notification spacing on `alarms[n0..]`
            {
                let mut by_dev: BTreeMap<String, Vec<usize>> = BTreeMap::new();
                for idx in n0..alarms.len() {
                    let dev = net.device_of(&alarms[idx].resource);
                    by_dev.entry(dev).or_default().push(idx);
                }
                for (_, mut indices) in by_dev {
                    indices.sort_by(|&a, &b| {
                        alarms[a]
                            .raised_at
                            .cmp(&alarms[b].raised_at)
                            .then_with(|| alarms[a].id.cmp(&alarms[b].id))
                    });
                    let mut prev: Option<i64> = None;
                    for idx in indices {
                        if let Some(p) = prev {
                            let need =
                                p + (nrng.lognormal_median(cfg.alarms.element_notify_ms, 0.6) as i64);
                            if alarms[idx].raised_at < need {
                                let dur = alarms[idx].cleared_at.map(|c| c - alarms[idx].raised_at);
                                alarms[idx].raised_at = need;
                                if let Some(d) = dur {
                                    alarms[idx].cleared_at = Some(need + d);
                                }
                            }
                        }
                        prev = Some(alarms[idx].raised_at);
                    }
                }
            }

            // Cut incidents & ground truth per fault
            let bg_indices: Vec<usize> = (n0..alarms.len())
                .filter(|&idx| alarms[idx].mechanism == "background")
                .collect();
            for f in &fs {
                let mut caused_ids: BTreeSet<String> = BTreeSet::new();
                let mut mech_map: BTreeMap<String, String> = BTreeMap::new();
                for (fid, aid, m, _) in &caused {
                    if fid == &f.id {
                        caused_ids.insert(aid.clone());
                        mech_map.insert(aid.clone(), m.clone());
                    }
                }

                let mut mine: Vec<&AlarmRecord> = alarms[n0..]
                    .iter()
                    .filter(|a| caused_ids.contains(&a.id))
                    .collect();
                mine.sort_by_key(|a| a.raised_at);

                if !mine.is_empty() {
                    let mut grouped = Vec::new();
                    let mut stray = Vec::new();
                    for a in &mine {
                        if trng.next_f64() < cfg.tickets.misgroup_prob {
                            stray.push(a.id.clone());
                        } else {
                            grouped.push(a.id.clone());
                        }
                    }
                    let first = mine[0];
                    for &b_idx in &bg_indices {
                        let b = &alarms[b_idx];
                        if trng.next_f64() < cfg.tickets.background_attach_prob
                            && (b.raised_at - first.raised_at).abs() < 30 * MIN_MS
                        {
                            grouped.push(b.id.clone());
                        }
                    }
                    let opened = first.raised_at + trng.range_i64(2 * MIN_MS, 15 * MIN_MS);
                    let resolved = f.repair + trng.range_i64(5 * MIN_MS, 30 * MIN_MS);
                    let affecting = mine.iter().any(|a| a.service_affecting);
                    let priority = if affecting {
                        "P1"
                    } else if mine.len() > 5 {
                        "P2"
                    } else {
                        "P3"
                    };
                    let mut inc = Map::new();
                    inc.insert("id".to_string(), json!(format!("inc:{}", f.id)));
                    inc.insert("opened_at".to_string(), json!(opened));
                    inc.insert("priority".to_string(), json!(priority));
                    inc.insert("scenario".to_string(), json!(f.scenario));
                    inc.insert("split".to_string(), json!(sc.split));
                    inc.insert("synthetic".to_string(), json!(true));
                    inc.insert("alarms".to_string(), json!(grouped));
                    inc.insert(
                        "opening_note".to_string(),
                        json!(format!(
                            "Opened by NOC. {} alarms, first {} ({}) on {}.",
                            mine.len(),
                            first.alarm_type,
                            first.probable_cause,
                            first.resource
                        )),
                    );
                    if sc.split == "history" {
                        inc.insert("resolved_at".to_string(), json!(resolved));
                        inc.insert(
                            "root_cause_resource".to_string(),
                            json!(f.resources[0]),
                        );
                        inc.insert(
                            "resolution_note".to_string(),
                            json!(format!(
                                "Root cause: {} at {}.",
                                f.fault_type, f.resources[0]
                            )),
                        );
                    }
                    all_incidents.push(inc);

                    if !stray.is_empty() && trng.next_f64() < cfg.tickets.duplicate_incident_prob {
                        let mut dup = Map::new();
                        dup.insert("id".to_string(), json!(format!("inc:{}:dup", f.id)));
                        dup.insert("opened_at".to_string(), json!(opened));
                        dup.insert("priority".to_string(), json!("P3"));
                        dup.insert("scenario".to_string(), json!(f.scenario));
                        dup.insert("split".to_string(), json!(sc.split));
                        dup.insert("synthetic".to_string(), json!(true));
                        dup.insert("alarms".to_string(), json!(stray));
                        dup.insert(
                            "opening_note".to_string(),
                            json!("Opened by NOC. Duplicate unlinked incident."),
                        );
                        all_incidents.push(dup);
                    }
                }

                truth.push(json!({
                    "fault": f.id,
                    "scenario": scenario,
                    "split": sc.split,
                    "kind": sc.kind,
                    "fault_type": f.fault_type,
                    "resources": f.resources,
                    "onset": f.onset,
                    "repair": f.repair,
                    "change": f.change,
                    "alarms": caused_ids.into_iter().collect::<Vec<_>>(),
                    "alarm_mechanisms": mech_map,
                    "window": [window.0, window.1],
                }));
            }

            let n_unrelated = crng.poisson(cfg.changes.unrelated_per_slot);
            let all_devs: Vec<String> = net.device_role.keys().cloned().collect();
            for _ in 0..n_unrelated {
                let at = crng.range_i64(window.0, window.1);
                let target = crng.choose(&all_devs).clone();
                let mut cnode = Map::new();
                cnode.insert(
                    "id".to_string(),
                    json!(format!("chg:{}:u{}", scenario, changes.len())),
                );
                cnode.insert("kind".to_string(), json!("configuration"));
                cnode.insert("planned_start".to_string(), json!(at - 10 * MIN_MS));
                cnode.insert("actual_start".to_string(), json!(at));
                cnode.insert("outcome".to_string(), json!("success"));
                cnode.insert("target".to_string(), json!(target));
                changes.push(cnode);
            }

            i += 1;
        }
    }

    // Emit nodes and edges into `net.g`
    for a in &alarms {
        let mut aprops = Map::new();
        aprops.insert("alarm_type".to_string(), json!(a.alarm_type));
        aprops.insert("perceived_severity".to_string(), json!(a.perceived_severity));
        aprops.insert("probable_cause".to_string(), json!(a.probable_cause));
        aprops.insert("specific_problem".to_string(), json!(a.specific_problem));
        aprops.insert("service_affecting".to_string(), json!(a.service_affecting));
        aprops.insert("scenario".to_string(), json!(a.scenario));
        aprops.insert("raised_at".to_string(), json!(a.raised_at));
        aprops.insert("cleared_at".to_string(), json!(a.cleared_at));
        aprops.insert(
            "state".to_string(),
            json!(if a.cleared_at.is_none() {
                "raised"
            } else {
                "cleared"
            }),
        );
        aprops.insert("synthetic".to_string(), json!(true));
        net.g.add_node(&a.id, "Alarm", aprops);
        net.g.add_edge(&a.id, "RAISED_ON", &a.resource, Map::new());
    }

    for mut c in changes {
        let cid = c.remove("id").unwrap().as_str().unwrap().to_string();
        let target = c.remove("target").unwrap().as_str().unwrap().to_string();
        c.insert("synthetic".to_string(), json!(true));
        net.g.add_node(&cid, "Change", c);
        net.g.add_edge(&cid, "TARGETS", &target, Map::new());
    }

    for mut inc in all_incidents.clone() {
        let iid = inc.remove("id").unwrap().as_str().unwrap().to_string();
        let inc_alarms = inc.remove("alarms").unwrap();
        net.g.add_node(&iid, "Incident", inc);
        if let Some(arr) = inc_alarms.as_array() {
            for aid in arr {
                if let Some(s) = aid.as_str() {
                    net.g.add_edge(s, "PART_OF", &iid, Map::new());
                }
            }
        }
    }

    for t in &truth {
        let fid = t["fault"].as_str().unwrap();
        let mut fprops = Map::new();
        fprops.insert("fault_type".to_string(), t["fault_type"].clone());
        fprops.insert("onset".to_string(), t["onset"].clone());
        fprops.insert("scenario".to_string(), t["scenario"].clone());
        fprops.insert("synthetic".to_string(), json!(true));
        net.g.add_node(fid, "Fault", fprops);
        if let Some(res_arr) = t["resources"].as_array() {
            for r in res_arr {
                if let Some(rs) = r.as_str() {
                    net.g.add_edge(fid, "AT", rs, Map::new());
                }
            }
        }
        if let Some(chg) = t["change"].as_str() {
            net.g.add_edge(fid, "TRIGGERED_BY", chg, Map::new());
        }
    }

    for (fid, aid, mech, hops) in &caused {
        let mut cprops = Map::new();
        cprops.insert("via".to_string(), json!(mech));
        cprops.insert("hops".to_string(), json!(hops));
        net.g.add_edge(fid, "CAUSED", aid, cprops);
    }

    for (label, concepts) in &binding.labels {
        let sl = format!("schema:{}", label);
        let mut sprops = Map::new();
        sprops.insert("name".to_string(), json!(label));
        sprops.insert("synthetic".to_string(), json!(false));
        net.g.add_node(&sl, "SchemaLabel", sprops);
        for c in concepts {
            let oc = format!("onto:{}", c);
            if !net.g.nodes.contains_key(&oc) {
                let vocab = c.split(':').next().unwrap_or("");
                let mut oprops = Map::new();
                oprops.insert("concept".to_string(), json!(c));
                oprops.insert("vocabulary".to_string(), json!(vocab));
                oprops.insert("synthetic".to_string(), json!(false));
                net.g.add_node(&oc, "OntologyConcept", oprops);
            }
            net.g.add_edge(&sl, "GROUNDED_IN", &oc, Map::new());
        }
    }

    let mut shas = net.g.write(out_dir)?;

    // Write alarms.stream.jsonl
    let mut events: Vec<(i64, String, &'static str)> = Vec::new();
    for a in &alarms {
        events.push((a.raised_at, a.id.clone(), "raise"));
        if let Some(c) = a.cleared_at {
            events.push((c, a.id.clone(), "clear"));
        }
    }
    events.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.cmp(b.2)));
    let stream_path = out_dir.join("alarms.stream.jsonl");
    {
        let mut w = BufWriter::new(File::create(&stream_path)?);
        for (t, aid, ev) in &events {
            serde_json::to_writer(&mut w, &json!({"alarm": aid, "event": ev, "t": t}))?;
            w.write_all(b"\n")?;
        }
        w.flush()?;
    }
    shas.insert("alarms.stream.jsonl".to_string(), sha256_file(&stream_path)?);

    // Write ground_truth.jsonl
    let truth_path = out_dir.join("ground_truth.jsonl");
    {
        let mut w = BufWriter::new(File::create(&truth_path)?);
        for row in &truth {
            serde_json::to_writer(&mut w, row)?;
            w.write_all(b"\n")?;
        }
        w.flush()?;
    }
    shas.insert("ground_truth.jsonl".to_string(), sha256_file(&truth_path)?);

    // Write binding.json
    let binding_path = out_dir.join("binding.json");
    fs::write(&binding_path, serde_json::to_string_pretty(&binding)?)?;

    let mut label_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut synth_count = 0usize;
    for node in net.g.nodes.values() {
        if let Some(l) = node.get("label").and_then(|v| v.as_str()) {
            *label_counts.entry(l.to_string()).or_insert(0) += 1;
        }
        if node.get("synthetic").and_then(|v| v.as_bool()) == Some(true) {
            synth_count += 1;
        }
    }
    let mut rel_counts: BTreeMap<String, usize> = BTreeMap::new();
    for edge in &net.g.edges {
        if let Some(r) = edge.get("rel").and_then(|v| v.as_str()) {
            *rel_counts.entry(r.to_string()).or_insert(0) += 1;
        }
    }

    let synth_share = ((synth_count as f64) / (net.g.nodes.len().max(1) as f64) * 10000.0).round()
        / 10000.0;
    let manifest = json!({
        "generator": format!("samyama-sdg {}", env!("CARGO_PKG_VERSION")),
        "pack": format!("telecom {}", PACK_VERSION),
        "seed": seed,
        "config": cfg,
        "config_file": config_path.and_then(|p| p.file_name()).and_then(|s| s.to_str()),
        "inputs": {
            "topology": {
                "file": topo_path.file_name().and_then(|s| s.to_str()),
                "sha256": sha256_file(topo_path)?,
                "license": "CC BY 4.0 (Internet Topology Zoo)"
            },
            "ontology_builds": binding.inputs
        },
        "outputs": shas,
        "counts": {
            "labels": label_counts,
            "relations": rel_counts
        },
        "synthetic_share": synth_share,
        "faults": truth.len(),
        "alarms": alarms.len(),
        "incidents": all_incidents.len(),
        "noc_pop": noc_pop
    });
    fs::write(
        out_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;

    Ok(manifest)
}

pub fn validate_telecom(dir: &Path) -> Result<Value> {
    let mut nodes: BTreeMap<String, Value> = BTreeMap::new();
    for line in BufReader::new(File::open(dir.join("nodes.jsonl"))?).lines() {
        let l = line?;
        if !l.trim().is_empty() {
            let v: Value = serde_json::from_str(&l)?;
            if let Some(id) = v.get("id").and_then(|x| x.as_str()) {
                nodes.insert(id.to_string(), v);
            }
        }
    }
    let mut edges: Vec<Value> = Vec::new();
    for line in BufReader::new(File::open(dir.join("edges.jsonl"))?).lines() {
        let l = line?;
        if !l.trim().is_empty() {
            edges.push(serde_json::from_str(&l)?);
        }
    }
    let mut truth: Vec<Value> = Vec::new();
    for line in BufReader::new(File::open(dir.join("ground_truth.jsonl"))?).lines() {
        let l = line?;
        if !l.trim().is_empty() {
            truth.push(serde_json::from_str(&l)?);
        }
    }
    let binding: Binding = serde_json::from_str(&fs::read_to_string(dir.join("binding.json"))?)?;
    let manifest: Value = serde_json::from_str(&fs::read_to_string(dir.join("manifest.json"))?)?;

    let mut errors: Vec<String> = Vec::new();
    let mut allowed: HashMap<String, HashSet<String>> = HashMap::new();
    for (p, vs) in &binding.enums {
        allowed.insert(
            p.clone(),
            vs.iter().map(|v| v.value.clone()).collect(),
        );
    }

    let alarms: BTreeMap<String, &Value> = nodes
        .iter()
        .filter(|(_, n)| n.get("label").and_then(|x| x.as_str()) == Some("Alarm"))
        .map(|(k, v)| (k.clone(), v))
        .collect();

    for (aid, a) in &alarms {
        for (p, ok) in &allowed {
            let val = a.get(p).and_then(|x| x.as_str()).unwrap_or("");
            if !ok.contains(val) {
                errors.push(format!("{}: {}={:?} not in bound vocabulary", aid, p, val));
            }
        }
        if let (Some(r), Some(c)) = (
            a.get("raised_at").and_then(|x| x.as_i64()),
            a.get("cleared_at").and_then(|x| x.as_i64()),
        ) {
            if c < r {
                errors.push(format!("alarm {} cleared before raised", aid));
            }
        }
    }

    for e in &edges {
        let src = e.get("src").and_then(|x| x.as_str()).unwrap_or("");
        let dst = e.get("dst").and_then(|x| x.as_str()).unwrap_or("");
        if !nodes.contains_key(src) || !nodes.contains_key(dst) {
            errors.push(format!("dangling edge {} -> {}", src, dst));
        }
    }

    let skew = 4
        * manifest["config"]["alarms"]["clock_skew_ms"]
            .as_f64()
            .unwrap_or(3000.0) as i64;
    let onset: HashMap<String, i64> = truth
        .iter()
        .map(|t| {
            (
                t["fault"].as_str().unwrap().to_string(),
                t["onset"].as_i64().unwrap(),
            )
        })
        .collect();

    let mut mechanism: BTreeMap<String, String> = BTreeMap::new();
    for e in &edges {
        if e.get("rel").and_then(|x| x.as_str()) == Some("CAUSED") {
            let src = e["src"].as_str().unwrap();
            let dst = e["dst"].as_str().unwrap();
            let via = e.get("via").and_then(|x| x.as_str()).unwrap_or("");
            mechanism.insert(dst.to_string(), via.to_string());
            if let Some(a) = alarms.get(dst) {
                let raised = a["raised_at"].as_i64().unwrap_or(0);
                if raised < onset[src] - skew {
                    errors.push(format!("{} raised before fault {}", dst, src));
                }
            }
        }
    }

    let eval_count = truth
        .iter()
        .filter(|t| t["split"].as_str() == Some("eval"))
        .count();

    let report = json!({
        "summary": {
            "conformance_errors": errors.len(),
            "eval_incidents_scored": eval_count,
            "faults": truth.len(),
            "alarms": alarms.len()
        },
        "errors": errors
    });
    fs::write(
        dir.join("realism.json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    Ok(report)
}
