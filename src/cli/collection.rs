use crate::api::schema::*;

pub(super) fn run_collection_command(args: &[String]) -> std::io::Result<i32> {
    let Some(command) = args.first().map(String::as_str) else {
        return usage(2);
    };
    let method = match command {
        "list" => parse_list(&args[1..]).map(Method::CollectionList),
        "get" => one(&args[1..])
            .map(|collection_id| Method::CollectionGet(CollectionTarget { collection_id })),
        "create" => parse_create(&args[1..]).map(Method::CollectionCreate),
        "add" => two(&args[1..]).map(|(collection_id, pane_id)| {
            Method::CollectionAdd(CollectionAddParams {
                collection_id,
                pane_id,
            })
        }),
        "promote" => parse_promote(&args[1..]).map(Method::CollectionPromote),
        "select" => parse_select(&args[1..]).map(Method::CollectionSelect),
        "reorder" => parse_reorder(&args[1..]).map(Method::CollectionReorder),
        "archive" => two(&args[1..]).map(|(collection_id, pane_id)| {
            Method::CollectionArchive(CollectionMemberTarget {
                collection_id,
                pane_id,
            })
        }),
        "restore" => two(&args[1..]).map(|(collection_id, pane_id)| {
            Method::CollectionRestore(CollectionMemberTarget {
                collection_id,
                pane_id,
            })
        }),
        "close" => parse_close(&args[1..]).map(Method::CollectionClose),
        "help" | "--help" | "-h" => return usage(0),
        _ => return usage(2),
    };

    match method {
        Ok(method) => super::runtime::collection(method),
        Err(message) => {
            eprintln!("{message}");
            usage(2)
        }
    }
}

fn parse_list(args: &[String]) -> Result<CollectionListParams, String> {
    let mut workspace_id = None;
    let mut tab_id = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                workspace_id = Some(required_value(args, index, "--workspace")?);
                index += 2;
            }
            "--tab" => {
                tab_id = Some(required_value(args, index, "--tab")?);
                index += 2;
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }
    Ok(CollectionListParams {
        workspace_id,
        tab_id,
    })
}

fn parse_create(args: &[String]) -> Result<CollectionCreateParams, String> {
    let mut target_pane_id = None;
    let mut direction = None;
    let mut ratio = None;
    let mut label = None;
    let mut focus = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--target-pane" => {
                target_pane_id = Some(required_value(args, index, "--target-pane")?);
                index += 2;
            }
            "--direction" => {
                direction = Some(parse_direction(&required_value(
                    args,
                    index,
                    "--direction",
                )?)?);
                index += 2;
            }
            "--ratio" => {
                ratio = Some(parse_ratio(&required_value(args, index, "--ratio")?)?);
                index += 2;
            }
            "--label" => {
                label = Some(required_value(args, index, "--label")?);
                index += 2;
            }
            "--focus" => {
                focus = true;
                index += 1;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }
    Ok(CollectionCreateParams {
        target_pane_id: target_pane_id.ok_or("missing --target-pane")?,
        direction: direction.ok_or("missing --direction")?,
        ratio,
        label,
        focus,
    })
}

fn parse_promote(args: &[String]) -> Result<CollectionPromoteParams, String> {
    let pane_id = args
        .first()
        .filter(|value| !value.starts_with('-'))
        .cloned()
        .ok_or("missing pane_id")?;
    let mut target_pane_id = None;
    let mut direction = None;
    let mut ratio = None;
    let mut focus = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--target-pane" => {
                target_pane_id = Some(required_value(args, index, "--target-pane")?);
                index += 2;
            }
            "--direction" => {
                direction = Some(parse_direction(&required_value(
                    args,
                    index,
                    "--direction",
                )?)?);
                index += 2;
            }
            "--ratio" => {
                ratio = Some(parse_ratio(&required_value(args, index, "--ratio")?)?);
                index += 2;
            }
            "--focus" => {
                focus = true;
                index += 1;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }
    Ok(CollectionPromoteParams {
        pane_id,
        target_pane_id: target_pane_id.ok_or("missing --target-pane")?,
        direction: direction.ok_or("missing --direction")?,
        ratio,
        focus,
    })
}

fn parse_select(args: &[String]) -> Result<CollectionSelectParams, String> {
    if args.len() < 2 {
        return Err("expected collection_id and pane_id".into());
    }
    let mut focus = false;
    for arg in &args[2..] {
        match arg.as_str() {
            "--focus" => focus = true,
            "--no-focus" => focus = false,
            _ => return Err(format!("unknown option: {arg}")),
        }
    }
    Ok(CollectionSelectParams {
        collection_id: args[0].clone(),
        pane_id: args[1].clone(),
        focus,
    })
}

fn parse_reorder(args: &[String]) -> Result<CollectionReorderParams, String> {
    if args.len() != 4 || args[2] != "--index" {
        return Err("usage: vqro collection reorder <collection_id> <pane_id> --index N".into());
    }
    Ok(CollectionReorderParams {
        collection_id: args[0].clone(),
        pane_id: args[1].clone(),
        index: args[3].parse().map_err(|_| "invalid index")?,
    })
}

fn parse_close(args: &[String]) -> Result<CollectionCloseParams, String> {
    let collection_id = args
        .first()
        .filter(|value| !value.starts_with('-'))
        .cloned()
        .ok_or("missing collection_id")?;
    let disposition = match &args[1..] {
        [] => None,
        [flag] if flag == "--cascade-close" => Some(CollectionCloseDisposition::CascadeClose),
        [flag] if flag == "--promote-members" => Some(CollectionCloseDisposition::PromoteMembers),
        _ => return Err("choose at most one of --cascade-close or --promote-members".into()),
    };
    Ok(CollectionCloseParams {
        collection_id,
        disposition,
    })
}

fn required_value(args: &[String], index: usize, name: &str) -> Result<String, String> {
    args.get(index + 1)
        .filter(|value| !value.starts_with("--"))
        .cloned()
        .ok_or_else(|| format!("missing value for {name}"))
}

fn parse_direction(value: &str) -> Result<SplitDirection, String> {
    match value {
        "right" => Ok(SplitDirection::Right),
        "down" => Ok(SplitDirection::Down),
        _ => Err("direction must be right or down".into()),
    }
}

fn parse_ratio(value: &str) -> Result<f32, String> {
    let value = value.parse::<f32>().map_err(|_| "invalid ratio")?;
    value
        .is_finite()
        .then_some(value)
        .ok_or_else(|| "invalid ratio".into())
}

fn one(args: &[String]) -> Result<String, String> {
    if args.len() == 1 {
        Ok(args[0].clone())
    } else {
        Err("expected one ID".into())
    }
}

fn two(args: &[String]) -> Result<(String, String), String> {
    if args.len() == 2 {
        Ok((args[0].clone(), args[1].clone()))
    } else {
        Err("expected two IDs".into())
    }
}

fn usage(code: i32) -> std::io::Result<i32> {
    eprintln!("vqro collection commands: list, get, create, add, promote, select, reorder, archive, restore, close");
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn parses_collection_create_promote_and_close() {
        let create = parse_create(&strings(&[
            "--target-pane",
            "w1:p1",
            "--direction",
            "right",
            "--ratio",
            "0.4",
            "--label",
            "helpers",
            "--no-focus",
        ]))
        .expect("create");
        assert_eq!(create.target_pane_id, "w1:p1");
        assert_eq!(create.ratio, Some(0.4));
        assert!(!create.focus);

        let promote = parse_promote(&strings(&[
            "w1:p2",
            "--target-pane",
            "w1:p1",
            "--direction",
            "down",
            "--focus",
        ]))
        .expect("promote");
        assert_eq!(promote.pane_id, "w1:p2");
        assert!(promote.focus);

        let close = parse_close(&strings(&["collection_1", "--promote-members"])).expect("close");
        assert_eq!(
            close.disposition,
            Some(CollectionCloseDisposition::PromoteMembers)
        );
    }

    #[test]
    fn collection_parsers_reject_ambiguous_or_invalid_mutations() {
        assert!(parse_close(&strings(&[
            "collection_1",
            "--cascade-close",
            "--promote-members"
        ]))
        .is_err());
        assert!(parse_create(&strings(&["--target-pane", "p1", "--direction", "left"])).is_err());
        assert!(parse_create(&strings(&["--target-pane", "--direction", "right"])).is_err());
        assert!(parse_reorder(&strings(&["collection_1", "p1", "--index", "nope"])).is_err());
        assert!(parse_select(&strings(&["collection_1"])).is_err());
    }
}
