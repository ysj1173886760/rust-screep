use std::{
    cell::RefCell,
    collections::{hash_map::Entry, HashMap, HashSet},
};

use js_sys::{JsString, Object, Reflect};
use log::*;
use screeps::{
    constants::{ErrorCode, Part, ResourceType},
    enums::StructureObject,
    find, game,
    local::ObjectId,
    objects::{Creep, Source, StructureController, StructureSpawn, ConstructionSite},
    prelude::*,
    HasId, // Add this import at the top of the file
    MaybeHasId, // Add MaybeHasId to the import
};
use wasm_bindgen::prelude::*;

mod logging;

// Define CreepRole enum
#[derive(Clone, Debug)]
enum CreepRole {
    Builder,
    Worker,
}

// Update CreepTarget enum
#[derive(Clone)]
enum CreepTarget {
    Upgrade(ObjectId<StructureController>),
    Harvest(ObjectId<Source>),
    Build(ObjectId<ConstructionSite>),
    FillSpawn(ObjectId<StructureSpawn>),
}

// Update thread_local storage to include role
thread_local! {
    static CREEP_INFO: RefCell<HashMap<String, (CreepRole, Option<CreepTarget>)>> = RefCell::new(HashMap::new());
}

static INIT_LOGGING: std::sync::Once = std::sync::Once::new();

// add wasm_bindgen to any function you would like to expose for call from js
// to use a reserved name as a function name, use `js_name`:
#[wasm_bindgen(js_name = loop)]
pub fn game_loop() {
    INIT_LOGGING.call_once(|| {
        // show all output of Info level, adjust as needed
        logging::setup_logging(logging::Info);
    });

    debug!("loop starting! CPU: {}", game::cpu::get_used());

    CREEP_INFO.with(|creep_info_refcell| {
        let mut creep_info = creep_info_refcell.borrow_mut();
        debug!("running creeps");
        for creep in game::creeps().values() {
            run_creep(&creep, &mut creep_info);
        }
    });

    debug!("running spawns");
    let mut additional = 0;
    for spawn in game::spawns().values() {
        debug!("running spawn {}", spawn.name());

        let body = [Part::Move, Part::Move, Part::Carry, Part::Work];
        if spawn.room().unwrap().energy_available() >= body.iter().map(|p| p.cost()).sum() {
            let name_base = game::time();
            let name = format!("{}-{}", name_base, additional);
            let role = if additional % 2 == 0 { CreepRole::Builder } else { CreepRole::Worker };
            
            match spawn.spawn_creep(&body, &name) {
                Ok(()) => {
                    CREEP_INFO.with(|creep_info_refcell| {
                        let mut creep_info = creep_info_refcell.borrow_mut();
                        creep_info.insert(name.clone(), (role, None));
                    });
                    additional += 1;
                },
                Err(e) => warn!("couldn't spawn: {:?}", e),
            }
        }
    }

    // memory cleanup; memory gets created for all creeps upon spawning, and any time move_to
    // is used; this should be removed if you're using RawMemory/serde for persistence
    if game::time() % 1000 == 0 {
        info!("running memory cleanup");
        let mut alive_creeps = HashSet::new();
        // add all living creep names to a hashset
        for creep_name in game::creeps().keys() {
            alive_creeps.insert(creep_name);
        }

        // grab `Memory.creeps` (if it exists)
        if let Ok(memory_creeps) = Reflect::get(&screeps::memory::ROOT, &JsString::from("creeps")) {
            // convert from JsValue to Object
            let memory_creeps: Object = memory_creeps.unchecked_into();
            // iterate memory creeps
            for creep_name_js in Object::keys(&memory_creeps).iter() {
                // convert to String (after converting to JsString)
                let creep_name = String::from(creep_name_js.dyn_ref::<JsString>().unwrap());

                // check the HashSet for the creep name, deleting if not alive
                if !alive_creeps.contains(&creep_name) {
                    info!("deleting memory for dead creep {}", creep_name);
                    let _ = Reflect::delete_property(&memory_creeps, &creep_name_js);
                }
            }
        }
    }

    info!("sheep done! cpu: {}", game::cpu::get_used())
}

fn run_creep(creep: &Creep, creep_info: &mut HashMap<String, (CreepRole, Option<CreepTarget>)>) {
    if creep.spawning() {
        return;
    }
    let name = creep.name();
    debug!("running creep {}", name);

    let (role, target) = creep_info.entry(name.clone())
        .or_insert_with(|| (CreepRole::Worker, None));

    // Function to make the creep say its role
    let say_role = |creep: &Creep, role: &CreepRole| {
        let role_name = match role {
            CreepRole::Builder => "Builder",
            CreepRole::Worker => "Worker",
        };
        creep.say(role_name, false);
    };

    match target {
        Some(CreepTarget::Upgrade(controller_id)) if creep.store().get_used_capacity(Some(ResourceType::Energy)) > 0 => {
            say_role(creep, role);
            if let Some(controller) = controller_id.resolve() {
                creep
                    .upgrade_controller(&controller)
                    .unwrap_or_else(|e| match e {
                        ErrorCode::NotInRange => {
                            let _ = creep.move_to(&controller);
                        }
                        _ => {
                            warn!("couldn't upgrade: {:?}", e);
                            *target = None;
                        }
                    });
            } else {
                *target = None;
            }
        }
        Some(CreepTarget::Harvest(source_id)) if creep.store().get_free_capacity(Some(ResourceType::Energy)) > 0 => {
            say_role(creep, role);
            if let Some(source) = source_id.resolve() {
                if creep.pos().is_near_to(source.pos()) {
                    creep.harvest(&source).unwrap_or_else(|e| {
                        warn!("couldn't harvest: {:?}", e);
                        *target = None;
                    });
                } else {
                    let _ = creep.move_to(&source);
                }
            } else {
                *target = None;
            }
        }
        Some(CreepTarget::Build(site_id)) if creep.store().get_used_capacity(Some(ResourceType::Energy)) > 0 => {
            say_role(creep, role);
            if let Some(site) = site_id.resolve() {
                creep.build(&site).unwrap_or_else(|e| match e {
                    ErrorCode::NotInRange => {
                        let _ = creep.move_to(&site);
                    }
                    _ => {
                        warn!("couldn't build: {:?}", e);
                        *target = None;
                    }
                });
            } else {
                *target = None;
            }
        }
        Some(CreepTarget::FillSpawn(spawn_id)) if creep.store().get_used_capacity(Some(ResourceType::Energy)) > 0 => {
            say_role(creep, role);
            if let Some(spawn) = spawn_id.resolve() {
                creep.transfer(&spawn, ResourceType::Energy, None).unwrap_or_else(|e| match e {
                    ErrorCode::NotInRange => {
                        let _ = creep.move_to(&spawn);
                    }
                    _ => {
                        warn!("couldn't transfer energy: {:?}", e);
                        *target = None;
                    }
                });
            } else {
                *target = None;
            }
        }
        _ => {
            // No target or invalid target, find a new one
            let room = creep.room().expect("couldn't resolve creep room");
            
            if creep.store().get_used_capacity(Some(ResourceType::Energy)) > 0 {
                match role {
                    CreepRole::Builder => {
                        if let Some(site) = room.find(find::CONSTRUCTION_SITES, None).first() {
                            if let Some(id) = site.try_id() {
                                *target = Some(CreepTarget::Build(id));
                                say_role(creep, role);
                            } else {
                                warn!("Construction site has no id");
                            }
                        } else if let Some(controller) = room.controller() {
                            *target = Some(CreepTarget::Upgrade(controller.id()));
                            say_role(creep, role);
                        }
                    }
                    CreepRole::Worker => {
                        if let Some(spawn) = room.find(find::MY_SPAWNS, None).first() {
                            if spawn.store().get_free_capacity(Some(ResourceType::Energy)) > 0 {
                                *target = Some(CreepTarget::FillSpawn(spawn.id()));
                                say_role(creep, role);
                            } else if let Some(controller) = room.controller() {
                                *target = Some(CreepTarget::Upgrade(controller.id()));
                                say_role(creep, role);
                            }
                        }
                    }
                }
            } else if let Some(source) = room.find(find::SOURCES_ACTIVE, None).first() {
                *target = Some(CreepTarget::Harvest(source.id()));
                say_role(creep, role);
            }
        }
    }
}