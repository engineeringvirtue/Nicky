extern crate serde;
extern crate serde_derive;

extern crate regex;
extern crate parking_lot;
extern crate typemap;
extern crate rayon;
extern crate kankyo;

extern crate serenity;
extern crate rustbreak;

use std::{sync::{Arc, atomic::{AtomicUsize, Ordering}}, collections::HashMap, thread, time::Duration};

use serde_derive::{Deserialize, Serialize};

use regex::Regex;
use parking_lot::RwLockReadGuard;
use rayon::prelude::*;

use serenity::prelude::*;
use serenity::framework::*;
use serenity::model::{permissions::Permissions, guild::{Guild, Member}, channel::{Message}, id::*};

use rustbreak::*;

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Data {
    prefixes: HashMap<u64, String>
}

type DB = Arc<FileDatabase<Data, rustbreak::deser::Bincode>>;

struct KDB;

impl typemap::Key for KDB {
    type Value = DB;
}

trait GetDB {
    fn get_db(&self) -> DB;
}

impl GetDB for Context {
    fn get_db(&self) -> DB {
        self.data.lock().get::<KDB>().unwrap().clone()
    }
}

struct Handler;

impl EventHandler for Handler {

}

const DB_PATH: &str = "db.txt";
const ADMIN_PERM: Permissions = Permissions::ADMINISTRATOR;

const PROGRESS_DONE: char = '█';
const PROGRESS_TBD: char = '░';

struct DatabaseSaver {
    db: DB
}

impl DatabaseSaver {
    fn start(self) {
        thread::spawn(move || {
            loop {
                self.db.save().unwrap();
                thread::sleep(Duration::new(8, 0));
            };
        });
    }
}

enum UserSpec {
    Include(Vec<UserId>),
    Exclude(Vec<UserId>),
    Everyone
}

impl UserSpec {
    fn new(mut args: standard::Args) -> Result<Self, standard::CommandError> {
        match args.clone().multiple::<UserId>() {
            Ok(inc) => Ok(UserSpec::Include(inc)),
            Err(_) => {
                if args.single::<String>().ok() == Some("--".to_owned()) {
                    let users = args.multiple::<UserId>()
                        .map_err(|_| "When using --, please specify users to exclude.")?;

                    Ok(UserSpec::Exclude(users))
                } else {
                    Ok(UserSpec::Everyone)
                }
            }
        }
    }

    fn get_members(self, guild: RwLockReadGuard<Guild>) -> Result<Vec<Member>, standard::CommandError> {
        match self {
            UserSpec::Include(inc) => {
                let mut members = Vec::new();

                for i in inc.iter() {
                    if let Some(x) = guild.members.get(i) {
                        members.push(x.clone());
                    }
                }

                Ok(members)
            },

            UserSpec::Exclude(ex) => {
                Ok(UserSpec::Everyone.get_members(guild)?.into_par_iter().filter(|x| {
                    !ex.par_iter().find_any(|exx| **exx == x.user_id()).is_some()
                }).collect())
            },

            UserSpec::Everyone => Ok(guild.members.values().map(|x| x.clone()).collect())
        }
    }

    fn nick_members<F: Fn(&str) -> String + Sync>
    (self, guild: RwLockReadGuard<Guild>, f: F, msg: &Message) -> Result<(), standard::CommandError> {

        let channel = msg.channel().unwrap();
        let reply = channel.send_message(|x| x.content("Loading..."))?;

        let set_stat = |new_stat: &str| -> Result<(), standard::CommandError> {
            channel.edit_message(reply.id, |x| x.content(new_stat))?;
            Ok(())
        };

        set_stat("Retrieving members...")?;
        let mem = self.get_members(guild)?;

        let members = mem.len();

        let progress = AtomicUsize::new(0);
        let update_progress = |name: &str| {
            let new_progress = progress.load(Ordering::Relaxed)+1;
            progress.store(new_progress, Ordering::Relaxed);

            let progress_perc: f64 = (new_progress as f64)/(members as f64);

            let mut progressbar = String::new();
            let progressbar_num = (progress_perc*12.0).round() as usize;

            for _ in 0..progressbar_num {
                progressbar.push(PROGRESS_DONE);
            }

            for _ in 0..(12 - progressbar_num) {
                progressbar.push(PROGRESS_TBD);
            }

            let _ = set_stat(&format!("Nicknaming... {} {:.0}% / {}", progressbar, progress_perc*100.0, name));
        };

        mem.into_par_iter().for_each(|x: Member| {
            let name = x.display_name().to_owned().to_string();
            let new_nick = f(&name);

            if let Err(err) = x.edit(|e| e.nickname(&new_nick)) {
                update_progress(&format!("__***Error whilst renaming {}: {}***__", &name, err))
            } else {
                update_progress(&name);
            }
        });

        set_stat("Done!")?;
        Ok(())
    }
}

fn main() {
    kankyo::load().unwrap();

    let database: DB =
        Arc::new({
            let db = FileDatabase::from_path(DB_PATH,
                                                 Data {prefixes: HashMap::new()}).unwrap();

            let _ = db.load();
            db
        });

    let tok = std::env::var("TOKEN").expect("TOKEN is not in .env!");
    let mut client = Client::new(&tok, Handler).unwrap();

    client.data.lock().insert::<KDB>(database.clone());
    DatabaseSaver {db: database}.start();

    client.with_framework({ StandardFramework::new()
        .configure(|x|
            x.on_mention(true).dynamic_prefix(|ctx, msg| {
                msg.guild_id.and_then(|x|
                    ctx.get_db().borrow_data().unwrap().prefixes.get(x.as_u64())
                        .map(|x| x.to_owned())
                )
            })
        )

        .command("prefix", |cmd|
            cmd.guild_only(true).desc("Set the prefix of Nicky")
                .required_permissions(ADMIN_PERM)
                .exec(|ctx, msg, mut args| {
                    let a = args.single::<String>()?;

                    let l = a.len();
                    if l > 0 && l < 80 {
                        let gid = msg.guild_id.unwrap();
                        ctx.get_db().write(|x| {
                            x.prefixes.insert(*gid.as_u64(), a.clone());
                        })?;

                        msg.reply(&format!("Prefix has been updated to {}!", a))?;

                        Ok(())
                    } else {
                        Err("Make sure your prefix is within reasonable length!".into())
                    }
                })
        )

        .simple_bucket("long", 1)
        .group("Nickname Commands", |x|
            x.desc("Nickname commands go here - all require Administrator permission")
                .bucket("long")
                .required_permissions(ADMIN_PERM).guild_only(true)
                .command("prepend", |x|
                    x.desc("Prepend text to nicknames").usage("prepend \"prefix\" @include -- @exclude")
                        .exec(|_ctx, msg, mut args| {
                            let prepend_str = args.single_quoted::<String>().map_err(|_| "No prefix found!")?;
                            let spec = UserSpec::new(args)?;

                            spec.nick_members(msg.guild().unwrap().read(), |x| {
                                format!("{}{}", prepend_str, x)
                            }, msg)
                        })
                ).command("append", |x|
                    x.desc("Append text to nicknames").usage("append \"prefix\" @include -- @exclude")
                        .exec(|_ctx, msg, mut args| {
                            let append_str = args.single_quoted::<String>().map_err(|_| "No affix found!")?;
                            let spec = UserSpec::new(args)?;

                            spec.nick_members(msg.guild().unwrap().read(), |x| {
                                format!("{}{}", x, append_str)
                            }, msg)
                        })
                ).command("set", |x|
                    x.desc("Set nicknames").usage("set \"new nickname\" @include -- @exclude")
                        .exec(|_ctx, msg, mut args| {
                            let set_str = args.single_quoted::<String>().map_err(|_| "No nickname found!")?;
                            let spec = UserSpec::new(args)?;

                            spec.nick_members(msg.guild().unwrap().read(), |_| {
                                set_str.clone()
                            }, msg)
                        })
                ).command("reset", |x|
                    x.desc("Reset nicknames").usage("reset @include -- @exclude")
                        .exec(|_ctx, msg, args| {
                            let spec = UserSpec::new(args)?;

                            spec.nick_members(msg.guild().unwrap().read(), |_| {
                                "".to_owned()
                            }, msg)
                        })
                ).command("replace", |x|
                    x.desc("Replace things in nicknames").usage("replace \"old value\" \"new value\" @include -- @exclude")
                        .exec(|_ctx, msg, mut args| {
                            let old_str = args.single_quoted::<String>().map_err(|_| "No old value found!")?;
                            let new_str = args.single_quoted::<String>().map_err(|_| "No new value found!")?;

                            let spec = UserSpec::new(args)?;

                            spec.nick_members(msg.guild().unwrap().read(), |x| {
                                x.replace(&old_str, &new_str)
                            }, msg)
                        })
                ).command("replace-regex", |x|
                    x.desc("Replace things in nicknames using regex")
                        .usage("replace-regex \"(old regex)\" \"new value\" @include -- @exclude")
                        .exec(|_ctx, msg, mut args| {
                            let regex = Regex::new(&args.single_quoted::<String>().map_err(|_| "No regex found!")?)?;
                            let new = args.single_quoted::<String>().map_err(|_| "No new value found!")?;

                            let spec = UserSpec::new(args)?;

                            spec.nick_members(msg.guild().unwrap().read(), |x| {
                                regex.replace(x, new.as_str()).to_owned().to_string()
                            }, msg)
                        })
                )
        )

        .customised_help(standard::help_commands::with_embeds, |x|
            x.individual_command_tip("Use ``help <command>`` if you're having trouble with a particular command.")
                .ungrouped_label("Misc."))

        .on_dispatch_error(|_ctx, msg, err| {
            let _ = match err {
                standard::DispatchError::LackOfPermissions(_) => {
                    msg.reply("Make sure you are an administrator!").ok()
                },
                standard::DispatchError::RateLimited(secs) => {
                    msg.reply(&format!("You must wait {} before doing that again!", secs)).ok()
                },
                _ => None
            };
        })

        .after(|_ctx, msg, cmd, res| {
            if let Err(standard::CommandError(x)) = res {
                let _ = msg.reply(&format!("{} Try ``help {}``.", x, cmd));
            }
        })
    });

    client.start_autosharded().unwrap();
}
