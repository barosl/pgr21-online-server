use websocket::{Server, Receiver, Message, WebSocketStream};
use websocket::Sender as SenderTrait;
use websocket::server::sender::Sender;
use std::thread::{spawn, sleep};
use rustc_serialize::json;
use std::sync::{Arc, Mutex};
use std::default::Default;
use std::collections::VecMap;
use time::SteadyTime;
use time::Duration;
use std::time::Duration as StdDuration;
use std::fs::File;
use toml;
use std::io::Read;
use rand;
use crypto::sha1::Sha1;
use crypto::digest::Digest;

#[derive(Clone)]
struct Unit {
    id: i32,
    x: i32,
    y: i32,
    speed: (i32, i32),
    cooldown: SteadyTime,
    direction: (i32, i32),
    name: String,
    img: String,
    text: String,
    style: String,
}

#[derive(RustcDecodable, RustcEncodable, Default)]
struct Msg {
    cmd: String,

    id: Option<i32>,
    x: Option<i32>,
    y: Option<i32>,

    speed: Option<i32>,

    name: Option<String>,
    signature: Option<String>,
    img: Option<String>,
    text: Option<String>,
    style: Option<String>,
}

#[derive(Clone)]
enum Trigger {
    Move(i32, i32),
}

struct Map {
    width: i32,
    height: i32,

    vacants: Vec<bool>,
    units: Vec<Vec<i32>>,
    init_places: Vec<(i32, i32)>,
    triggers: Vec<Vec<Trigger>>,
}

struct SenderState {
    sender: Sender<WebSocketStream>,
    pinged: SteadyTime,
}

#[derive(Clone)]
struct GlobalState {
    map: Arc<Mutex<Map>>,
    units: Arc<Mutex<VecMap<Unit>>>,
    last_unit_id: Arc<Mutex<i32>>,
    wrs: Arc<Mutex<VecMap<Arc<Mutex<SenderState>>>>>,
    key: String,
    default_img: String,
    privileged: Vec<String>,
}

struct LocalState {
    unit_ids: Vec<i32>,
    wr: Arc<Mutex<SenderState>>,
    username: Option<String>,
}

#[allow(unused_must_use)]
fn send(wr: &Arc<Mutex<SenderState>>, msg: Msg) {
    wr.lock().unwrap().sender.send_message(Message::Text(json::encode(&msg).unwrap()));
}

#[allow(unused_must_use)]
fn broadcast(wrs: &Arc<Mutex<VecMap<Arc<Mutex<SenderState>>>>>, msg: Msg) {
    let msg = Message::Text(json::encode(&msg).unwrap());

    for wr in &*wrs.lock().unwrap() {
        wr.1.lock().unwrap().sender.send_message(msg.clone());
    }
}

fn on_msg(g_state: &GlobalState,
          l_state: &mut LocalState,
          msg: Msg,
         ) -> Result<(), String> {
    match &*msg.cmd {
        "login" => {
            match (msg.name, msg.signature) {
                (Some(name), Some(signature)) => {
                    let mut hasher = Sha1::new();
                    hasher.input_str(&*name);
                    hasher.input_str(&*g_state.key);
                    let hash = hasher.result_str();
                    if hash != signature {
                        return Err("Invalid signature".to_string());
                    }

                    l_state.username = Some(name);
                }

                _ => return Err("name and signature must be provided".to_string())
            }
        }

        "start" => {
            let unit_name = match l_state.username {
                Some(ref username) => username.clone(),
                None => return Err("Log in first".to_string()),
            };

            let unit_id = {
                let mut last_unit_id = g_state.last_unit_id.lock().unwrap();
                *last_unit_id += 1;
                *last_unit_id
            };

            let init_place = {
                let map = g_state.map.lock().unwrap();
                map.init_places[rand::random::<usize>() % map.init_places.len()]
            };

            let mut unit = Unit {
                id: unit_id,
                x: init_place.0,
                y: init_place.1,
                speed: (0, 0),
                direction: (0, 0),
                cooldown: SteadyTime::now(),
                name: unit_name,
                img: g_state.default_img.clone(),
                text: "".to_string(),
                style: "".to_string(),
            };

            if let &Some(ref username) = &l_state.username {
                if g_state.privileged.iter().any(|x| *x == *username) {
                    if let Some(x) = msg.x {
                        if let Some(y) = msg.y {
                            unit.x = x;
                            unit.y = y;
                        }
                    }

                    if let Some(img) = msg.img {
                        unit.img = img.clone();
                    }

                    if let Some(text) = msg.text {
                        unit.text = text.clone();
                    }

                    if let Some(style) = msg.style {
                        unit.style = style.clone();
                    }
                }
            }

            g_state.units.lock().unwrap().insert(unit_id as usize, unit.clone());

            {
                let mut map = g_state.map.lock().unwrap();
                let tile_idx = unit.x + unit.y * map.width;
                map.units[tile_idx as usize].push(unit.id);
            }

            l_state.unit_ids.push(unit_id);

            send(&l_state.wr, Msg {
                cmd: "you".to_string(),
                id: Some(unit_id),

                ..Default::default()
            });

            broadcast(&g_state.wrs, Msg {
                cmd: "unit".to_string(),
                id: Some(unit_id),
                x: Some(unit.x),
                y: Some(unit.y),
                name: Some(unit.name),
                img: Some(unit.img),
                text: Some(unit.text),
                style: Some(unit.style),

                ..Default::default()
            });

            for (unit_idx, unit) in g_state.units.lock().unwrap().iter() {
                if unit_id == unit_idx as i32 { continue; }

                send(&l_state.wr, Msg {
                    cmd: "unit".to_string(),
                    id: Some(unit_idx as i32),
                    x: Some(unit.x),
                    y: Some(unit.y),
                    name: Some(unit.name.clone()),
                    img: Some(unit.img.clone()),
                    text: Some(unit.text.clone()),
                    style: Some(unit.style.clone()),

                    ..Default::default()
                });
            }
        }

        "speed" => {
            let speed = match (msg.x, msg.y) {
                (Some(x), Some(y)) if x.abs() + y.abs() <= 1 => (x, y),
                _ => return Err("Invalid speed".to_string()),
            };

            let unit_id = match msg.id {
                Some(unit_id) => if l_state.unit_ids.iter().any(|x| *x == unit_id) {
                    unit_id
                } else {
                    return Err(format!("Invalid unit_id: {:?}", unit_id));
                },
                _ => return Err("msg.id not exists".to_string()),
            };

            {
                let mut units = g_state.units.lock().unwrap();

                let unit = match units.get_mut(&(unit_id as usize)) {
                    Some(unit) => unit,
                    None => return Err("unit not exists".to_string()),
                };

                unit.speed = speed;

                if speed != (0, 0) {
                    unit.direction = speed;
                }
            }
        }

        "click" => {
            let unit_id = match msg.id {
                Some(unit_id) => if l_state.unit_ids.iter().any(|x| *x == unit_id) {
                    unit_id
                } else {
                    return Err(format!("Invalid unit_id: {:?}", unit_id));
                },
                _ => return Err("msg.id not exists".to_string()),
            };

            {
                let mut units = g_state.units.lock().unwrap();

                let unit = match units.get_mut(&(unit_id as usize)) {
                    Some(unit) => unit,
                    None => return Err("unit not exists".to_string()),
                };

                if unit.direction != (0, 0) {
                    let x = unit.x + unit.direction.0;
                    let y = unit.y + unit.direction.1;

                    let map = g_state.map.lock().unwrap();
                    let tile_idx = x + y * map.width;

                    if tile_idx >= 0 && tile_idx < map.units.len() as i32 {
                        for unit_id in &map.units[tile_idx as usize] {
                            broadcast(&g_state.wrs, Msg {
                                cmd: "call".to_string(),
                                x: Some(unit.id),
                                y: Some(*unit_id),

                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }

        "remove" => {
            match msg.id {
                Some(unit_id) => {
                    if !l_state.unit_ids.iter().any(|x| *x == unit_id) {
                        return Err("Permission denied".to_string());
                    }
                    remove_unit(&g_state, unit_id);
                }
                None => return Err("No unit_id provided".to_string()),
            }
        }

        "ping" => {
            l_state.wr.lock().unwrap().pinged = SteadyTime::now();
        }

        "close" => {
            return Err("Manually closed".to_string());
        }

        _ => {
        }
    };

    Ok(())
}

fn remove_unit(g_state: &GlobalState, unit_id: i32) {
    let unit = g_state.units.lock().unwrap().remove(&(unit_id as usize)).unwrap();

    {
        let mut map = g_state.map.lock().unwrap();
        let tile_idx = (unit.x + unit.y * map.width) as usize;
        map.units[tile_idx].iter().position(|x| *x == unit.id).map(|idx| {
            map.units[tile_idx].remove(idx);
        });
    }

    broadcast(&g_state.wrs, Msg {
        cmd: "remove".to_string(),
        id: Some(unit_id),

        ..Default::default()
    });
}

#[derive(RustcDecodable)]
struct TiledLayer {
    data: Vec<i32>,
}

#[derive(RustcDecodable)]
struct TiledMap {
    width: i32,
    height: i32,
    layers: Vec<TiledLayer>,
}

fn load_map(fname: &str) -> Map {
    let mut text = String::new();
    File::open(fname).ok().expect("file not exists").read_to_string(&mut text).ok().expect("invalid file");
    let mut parser = toml::Parser::new(&*text);
    let toml = parser.parse().expect("invalid toml");

    let (file, vacant_tiles, init_places) = match toml.get("map") {
        Some(&toml::Value::Table(ref map)) => (
            match map.get("file") {
                Some(&toml::Value::String(ref file)) => file,
                _ => panic!("invalid map.file"),
            },

            match map.get("vacant_tiles") {
                Some(&toml::Value::Array(ref vacant_tiles)) => {
                    vacant_tiles.iter().map(|x| match x {
                        &toml::Value::Integer(y) => y as i32,
                        _ => panic!("invalid vacant_tiles")
                    }).collect::<Vec<i32>>()
                }
                _ => panic!("invalid map.vacant_tiles"),
            },

            match map.get("init_places") {
                Some(&toml::Value::Array(ref init_places)) => init_places.iter().map(|x| match x {
                    &toml::Value::Array(ref y) => {
                        let b: Vec<i32> = y.iter().map(|z| {
                            match z {
                                &toml::Value::Integer(ref a) => *a as i32,
                                _ => panic!("not integer"),
                            }
                        }).collect();

                        (b[0], b[1])
                    }
                    _ => panic!("not array"),
                }).collect(),
                _ => panic!("invalid map.vacant_tile"),
            },
        ),
        _ => panic!("invalid map"),
    };

    let mut text = String::new();
    File::open(file).unwrap().read_to_string(&mut text).unwrap();

    let tiled: TiledMap = json::decode(&*text).unwrap();
    let mut vacants: Vec<bool> = vec![true; (tiled.width * tiled.height) as usize];

    for layer in tiled.layers {
        for (i, tile) in layer.data.iter().enumerate() {
            if vacant_tiles.iter().all(|x| *x != *tile) && *tile != 0 {
                vacants[i] = false;
            }
        }
    }

    let mut triggers = vec![Vec::new(); (tiled.width * tiled.height) as usize];

    match toml.get("trigger") {
        Some(&toml::Value::Array(ref trigger)) => {
            for trigger in trigger {
                match trigger {
                    &toml::Value::Table(ref trigger) => {
                        match (trigger.get("type"), trigger.get("from"), trigger.get("to")) {
                            (Some(&toml::Value::String(ref type_)),
                             Some(&toml::Value::Array(ref from)),
                             Some(&toml::Value::Array(ref to))) => {
                                 match &**type_ {
                                     "move" => {
                                         let from: Vec<i32> = from.iter().map(|x| {
                                             match x {
                                                 &toml::Value::Integer(x) => x as i32,
                                                 _ => panic!("invalid from"),
                                             }
                                         }).collect();

                                         let to: Vec<i32> = to.iter().map(|x| {
                                             match x {
                                                 &toml::Value::Integer(x) => x as i32,
                                                 _ => panic!("invalid to"),
                                             }
                                         }).collect();

                                         let tile_idx = (from[0] + from[1] * tiled.width) as usize;
                                         triggers[tile_idx].push(Trigger::Move(to[0], to[1]));
                                     }

                                     _ => panic!("invalid type"),
                                 }

                             }

                            _ => panic!("invalid type or from or to"),
                        }
                    }

                    _ => panic!("invalid trigger"),
                }
            }
        }

        _ => panic!("invalid trigger"),
    }

    let units = vec![Vec::new(); (tiled.width * tiled.height) as usize];

    Map {
        width: tiled.width,
        height: tiled.height,

        vacants: vacants,
        units: units,

        init_places: init_places,
        triggers: triggers,
    }
}

macro_rules! toml_get {
    ($toml: expr, $name: expr, $type_: path) => {
        match $toml.get($name).unwrap() {
            &$type_(ref val) => {
                val.clone()
            }

            _ => panic!("Invalid TOML")
        }
    }
}

fn load_cfg(fname: &str) -> (u16, String, i32, String, Vec<String>) {
    let mut text = String::new();
    File::open(fname).unwrap().read_to_string(&mut text).unwrap();
    let toml = toml::Parser::new(&*text).parse().unwrap();

    let cfg = toml_get!(toml, "cfg", toml::Value::Table);
    let port = toml_get!(cfg, "port", toml::Value::Integer);
    let key = toml_get!(cfg, "key", toml::Value::String);
    let unit_speed = toml_get!(cfg, "unit_speed", toml::Value::Integer);
    let default_img = toml_get!(cfg, "default_img", toml::Value::String);
    let privileged = toml_get!(cfg, "privileged", toml::Value::Array).iter().map(|x| match *x {
        toml::Value::String(ref val) => val.clone(),
        _ => panic!("Invalid TOML"),
    }).collect();

    (port as u16, key, unit_speed as i32, default_img, privileged)
}

pub fn start() {
    let (port, key, unit_speed, default_img, privileged) = load_cfg("cfg.toml");

    let server = Server::bind(("0.0.0.0", port)).unwrap();

    let map = load_map("map.toml");

    let g_state = GlobalState {
        map: Arc::new(Mutex::new(map)),
        units: Arc::new(Mutex::new(VecMap::new())),
        last_unit_id: Arc::new(Mutex::new(0)),
        wrs: Arc::new(Mutex::new(VecMap::new())),
        key: key,
        default_img: default_img,
        privileged: privileged,
    };

    {
        let g_state = g_state.clone();

        spawn(move || {
            loop {
                let cur_time = SteadyTime::now();

                let mut msgs = Vec::new();

                for (unit_id, unit) in &mut *g_state.units.lock().unwrap() {
                    if unit.speed == (0, 0) || unit.cooldown > cur_time {
                        continue;
                    }

                    let mut new_x = unit.x + unit.speed.0;
                    let mut new_y = unit.y + unit.speed.1;

                    let (tile_idx, vacant) = {
                        let map = g_state.map.lock().unwrap();
                        let tile_idx = new_x + new_y * map.width;
                        if tile_idx >= 0 && tile_idx < map.vacants.len() as i32 {
                            (Some(tile_idx as usize), map.vacants[tile_idx as usize])
                        } else {
                            (None, false)
                        }
                    };

                    let mut should_move = false;
                    let mut speed = unit_speed;

                    if let Some(tile_idx) = tile_idx {
                        let map = g_state.map.lock().unwrap();
                        for trigger in &map.triggers[tile_idx] {
                            match trigger {
                                &Trigger::Move(x, y) => {
                                    should_move = true;

                                    new_x = x;
                                    new_y = y;

                                    speed = 0;
                                }
                            }
                        }
                    }

                    if should_move || vacant {
                        {
                            let mut map = g_state.map.lock().unwrap();

                            let prev_tile_idx = (unit.x + unit.y * map.width) as usize;

                            map.units[prev_tile_idx].iter().position(|x| *x == unit.id).map(|idx| {
                                map.units[prev_tile_idx].remove(idx);
                            });

                            map.units[tile_idx.unwrap()].push(unit.id);
                        }

                        unit.x = new_x;
                        unit.y = new_y;

                        unit.cooldown = cur_time + Duration::milliseconds(200);

                        msgs.push(Msg {
                            cmd: "move".to_string(),
                            id: Some(unit_id as i32),
                            x: Some(unit.x),
                            y: Some(unit.y),
                            speed: Some(speed),

                            ..Default::default()
                        });
                    }
                }

                for msg in msgs {
                    broadcast(&g_state.wrs, msg);
                }

                sleep(StdDuration::milliseconds(10));
            }
        });
    }

    {
        let g_state = g_state.clone();

        spawn(move || {
            loop {
                let cur_time = SteadyTime::now();

                for (_, wr) in g_state.wrs.lock().unwrap().iter_mut() {
                    let mut wr = wr.lock().unwrap();

                    if cur_time - wr.pinged >= Duration::seconds(30) {
                        use std::net::Shutdown::Both;
                        wr.sender.get_mut().shutdown(Both);
                    }
                }

                sleep(StdDuration::seconds(30));
            }
        });
    }

    let mut last_cli_id = 0;

    for sock in server {
        let g_state = g_state.clone();

        last_cli_id += 1;
        let cli_id = last_cli_id;

        spawn(move || {
            let sock = sock.unwrap().read_request().unwrap().accept().send().unwrap();

            let (mut wr, mut rd) = sock.split();

            let ip = wr.get_mut().peer_addr().unwrap();

            let wr = SenderState {
                sender: wr,
                pinged: SteadyTime::now(),
            };

            let mut l_state = LocalState {
                unit_ids: vec![],
                wr: Arc::new(Mutex::new(wr)),
                username: None,
            };

            g_state.wrs.lock().unwrap().insert(cli_id, l_state.wr.clone());

            for msg in rd.incoming_messages() {
                let msg = match msg {
                    Ok(msg) => msg,
                    Err(..) => break,
                };

                match msg {
                    Message::Text(text) => {
                        let msg: Msg = match json::decode(&*text) {
                            Ok(msg) => msg,
                            Err(..) => {
                                println!("Invalid message format");
                                break;
                            }
                        };

                        match on_msg(&g_state, &mut l_state, msg) {
                            Err(err) => {
                                println!("Client error: {}", err);
                                break;
                            }
                            _ => (),
                        }
                    }

                    _ => ()
                }
            }

            println!("Socket closed from {:?}", ip);

            for unit_id in l_state.unit_ids {
                remove_unit(&g_state, unit_id);
            }

            g_state.wrs.lock().unwrap().remove(&cli_id);
        });
    }
}
