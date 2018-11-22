extern crate serde;
extern crate serde_derive;

extern crate regex;
extern crate parking_lot;
extern crate typemap;
extern crate rayon;
extern crate kankyo;

extern crate serenity;
extern crate rustbreak;

use std::{sync::{Arc, atomic::{AtomicUsize, Ordering}}, collections::HashMap, thread, time::Duration, str::FromStr};

use serde_derive::{Deserialize, Serialize};

use regex::Regex;
use parking_lot::RwLockReadGuard;
use rayon::prelude::*;

use serenity::prelude::*;
use serenity::framework::*;
use serenity::model::{permissions::Permissions, guild::{Guild, Member, Role}, channel::{Message}, id::*};

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

#[derive(Debug, Clone)]
struct UserOrRole(UserId);

impl FromStr for UserOrRole {
    type Err = serenity::model::misc::UserIdParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        UserId::from_str(s).map(UserOrRole)
    }
}

impl UserOrRole {
    fn as_role(&self, guild: &RwLockReadGuard<Guild>) -> Option<Role> {
        guild.roles.get(&RoleId(*self.0.as_u64())).cloned()
    }

    fn get_members(self, guild: &RwLockReadGuard<Guild>) -> Vec<Member> {
        match self.as_role(guild) {
            None => guild.member(self.0).ok().map(|x| vec![x]).unwrap_or_else(|| Vec::new()),
            Some (x) => guild.members.par_iter().map(|(_, m)| m.clone()).filter(|m| m.roles.contains(&x.id)).collect()
        }
    }

    fn get_ids(self, guild: &RwLockReadGuard<Guild>) -> Vec<UserId> {
        match self.as_role(guild) {
            None => vec![self.0],
            Some (x) =>
                guild.members.par_iter().map(|(_, m)| m).filter(|m| m.roles.contains(&x.id))
                    .map(|x| x.user_id()).collect()
        }
    }
}

enum UserSpec {
    Include(Vec<UserOrRole>),
    Exclude(Vec<UserOrRole>),
    Everyone
}

impl UserSpec {
    fn new(mut args: standard::Args) -> Result<Self, standard::CommandError> {
        match args.clone().multiple::<UserOrRole>() {
            Ok(inc) => Ok(UserSpec::Include(inc)),
            Err(_) => {
                if args.single::<String>().ok() == Some("--".to_owned()) {
                    let users = args.multiple::<UserOrRole>()
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
                Ok(inc.into_par_iter().flat_map(|x: UserOrRole| x.get_members(&guild)).collect())
            },

            UserSpec::Exclude(ex) => {
                let ex_vec: Vec<UserId> = ex.into_par_iter().flat_map(|x| x.get_ids(&guild)).collect();
                Ok(UserSpec::Everyone.get_members(guild)?.into_par_iter().filter(|x| !ex_vec.contains(&x.user_id())).collect())
            },

            UserSpec::Everyone => Ok(guild.members.values().cloned().collect())
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

        mem.into_iter().for_each(|x: Member| {
            let name = x.display_name().to_owned().to_string();
            let new_nick = f(&name);

            if let Err(_) = x.edit(|e| e.nickname(&new_nick)) {
                update_progress(&format!("Perhaps I don't have permission to rename **{}**?", &name))
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
                let db = ctx.get_db();
                msg.guild_id.and_then(|x|
                    db.borrow_data().unwrap().prefixes.get(x.as_u64()).cloned()
                )
            })
        )

        .command("prefix", |cmd|
            cmd.guild_only(true).desc("Set the prefix of Nicky")
                .required_permissions(ADMIN_PERM)
                .exec(|ctx, msg, mut args| {
                    let a = args.single_quoted::<String>()?;

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

        .simple_bucket("long", 50)
        .group("Nickname Commands", |x|
            x.desc("Nickname commands go here - all require Administrator permission")
                .bucket("long")
                .required_permissions(ADMIN_PERM).guild_only(true)
                .command("prepend", |x|
                    x.desc("Prepend text to nicknames").usage("prepend \"prefix\" @include -- @exclude")
                        .exec(|_ctx, msg, mut args| {
                            let prepend_str = args.single_quoted::<String>().map_err(|_| "No prefix found!")?;
                            let spec = UserSpec::new(args)?;

                            let guild = msg.guild().unwrap();
                            spec.nick_members(guild.read(), |x| {
                                format!("{}{}", prepend_str, x)
                            }, msg)?;
                            Ok(())
                        })
                ).command("append", |x|
                    x.desc("Append text to nicknames").usage("append \"prefix\" @include -- @exclude")
                        .exec(|_ctx, msg, mut args| {
                            let append_str = args.single_quoted::<String>().map_err(|_| "No affix found!")?;
                            let spec = UserSpec::new(args)?;

                            let guild = msg.guild().unwrap();
                            spec.nick_members(guild.read(), |x| {
                                format!("{}{}", x, append_str)
                            }, msg)?;
                            Ok(())
                        })
                ).command("set", |x|
                    x.desc("Set nicknames").usage("set \"new nickname\" @include -- @exclude")
                        .exec(|_ctx, msg, mut args| {
                            let set_str = args.single_quoted::<String>().map_err(|_| "No nickname found!")?;
                            let spec = UserSpec::new(args)?;

                            let guild = msg.guild().unwrap();
                            spec.nick_members(guild.read(), |_| {
                                set_str.clone()
                            }, msg)?;
                            Ok(())
                        })
                ).command("reset", |x|
                    x.desc("Reset nicknames").usage("reset @include -- @exclude")
                        .exec(|_ctx, msg, args| {
                            let spec = UserSpec::new(args)?;

                            let guild = msg.guild().unwrap();
                            spec.nick_members(guild.read(), |_| {
                                "".to_owned()
                            }, msg)?;
                            Ok(())
                        })
                ).command("replace", |x|
                    x.desc("Replace things in nicknames").usage("replace \"old value\" \"new value\" @include -- @exclude")
                        .exec(|_ctx, msg, mut args| {
                            let old_str = args.single_quoted::<String>().map_err(|_| "No old value found!")?;
                            let new_str = args.single_quoted::<String>().map_err(|_| "No new value found!")?;

                            let spec = UserSpec::new(args)?;

                            let guild = msg.guild().unwrap();
                            spec.nick_members(guild.read(), |x| {
                                x.replace(&old_str, &new_str)
                            }, msg)?;
                            Ok(())
                        })
                ).command("replace-regex", |x|
                    x.desc("Replace things in nicknames using regex")
                        .usage("replace-regex \"(old regex)\" \"new value\" @include -- @exclude")
                        .exec(|_ctx, msg, mut args| {
                            let regex = Regex::new(&args.single_quoted::<String>().map_err(|_| "No regex found!")?)?;
                            let new = args.single_quoted::<String>().map_err(|_| "No new value found!")?;

                            let spec = UserSpec::new(args)?;

                            let guild = msg.guild().unwrap();
                            spec.nick_members(guild.read(), |x| {
                                regex.replace(x, new.as_str()).to_owned().to_string()
                            }, msg)?;
                            Ok(())
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
