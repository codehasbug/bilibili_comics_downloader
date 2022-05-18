use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::exit;
use qrcode::QrCode;
use std::time::Duration;
use colorful::Colorful;
use epub_builder::{EpubContent, ZipLibrary};
use indicatif::ProgressBar;
use tokio::sync::mpsc::Sender;

use crate::lib::cache::EpisodeCache;
use crate::lib::config::Config;
use crate::lib::network::{down_to, EpisodeInfo};

pub mod config;
pub mod network;
pub mod cache;
mod pdf;


fn delete_all_files(path: String) {
    // 递归删除文件夹下的所有文件
    let path = Path::new(&path);
    if path.is_dir() {
        for entry in path.read_dir().unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                delete_all_files(path.to_str().unwrap().to_string());
                std::fs::remove_dir(path).unwrap();
            } else {
                std::fs::remove_file(path).unwrap();
            }
        }
    }
}

fn bytes_with_unit(bytes: u64) -> String {
    let mut bytes = bytes;
    let mut unit = "B";
    if bytes > 1024 {
        bytes /= 1024;
        unit = "KB";
    }
    if bytes > 1024 {
        bytes /= 1024;
        unit = "MB";
    }
    if bytes > 1024 {
        bytes /= 1024;
        unit = "GB";
    }
    if bytes > 1024 {
        bytes /= 1024;
        unit = "TB";
    }
    format!("{} {}", bytes, unit)
}

fn get_dir_size(path: &str) -> u64 {
    let mut size = 0;
    for entry in std::fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            size += entry.metadata().unwrap().len();
        } else if entry.file_type().unwrap().is_dir() {
            size += get_dir_size(entry.path().to_str().unwrap());
        }
    }
    size
}

/// 输出配置信息
pub async fn info() {
    let config = config::Config::load();
    let mut log = paris::Logger::new();
    if let Some(user_info) = network::get_user_info(&config).await {
        log.info("登录信息有效！");
        log.info(format!("用户名：{}", user_info.name));
        log.info(format!("漫币余额：{}", user_info.coin));
    }
    log.info(format!("缓存目录：{}", config.cache_dir));
    log.info(format!("缓存目录大小：{}", bytes_with_unit(get_dir_size(config.cache_dir.as_str()))));
    log.info(format!("默认下载目录：{}", config.default_download_dir));
}


/// 清空缓存
pub fn clear() {
    let config = config::Config::load();
    let mut log = paris::Logger::new();
    log.info(format!("清空文件夹: {}", config.cache_dir));
    delete_all_files(config.cache_dir);
}

pub enum LoginMethod {
    SESSDATA(String),
    QRCODE,
}

pub async fn show_login_info() {
    let config = config::Config::load();
    let mut log = paris::Logger::new();
    if let Some(user_info) = network::get_user_info(&config).await {
        log.info("登录信息有效！");
        log.info(format!("用户名：{}", user_info.name));
        log.info(format!("漫币余额：{}", user_info.coin));
    } else {
        log.info("登录信息无效或未登录！");
    }
}


pub async fn login(method: LoginMethod) {
    let mut log = paris::Logger::new();
    let mut config = config::Config::load();
    match method {
        LoginMethod::SESSDATA(sessdata) => {
            config.sessdata = sessdata;
            if let Some(user_info) = network::get_user_info(&config).await {
                log.info("登录信息有效！");
                log.info(format!("用户名：{}", user_info.name));
                log.info(format!("漫币余额：{}", user_info.coin));
                config.save();
            } else {
                log.error("登录信息无效！");
            }
        }
        LoginMethod::QRCODE => {
            let (qr_data, oauth) = network::get_qr_data(&config).await;
            let code = QrCode::new(qr_data.to_owned()).unwrap();
            let image = code.render::<qrcode::render::unicode::Dense1x2>()
                .dark_color(qrcode::render::unicode::Dense1x2::Dark)
                .light_color(qrcode::render::unicode::Dense1x2::Light)
                .build();
            println!("{}", image);
            log.success("二维码已生成，请扫描二维码登录");
            log.info(format!("如果显示错误，请手动访问：{}", qr_data));
            log.loading("等待扫描...");
            let mut last_status = "NotScan";
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                match network::check_qr_status(&config, oauth.clone()).await {
                    network::QRStatus::NotScan => {
                        if last_status != "NotScan" {
                            log.done();
                            log.loading("等待扫描...");
                        }
                        last_status = "NotScan";
                    }
                    network::QRStatus::Scanning => {
                        if last_status != "Scanning" {
                            log.done();
                            log.loading("等待确认...");
                        }
                        last_status = "Scanning";
                    }
                    network::QRStatus::Complete(sessdata) => {
                        log.done();
                        log.success("登录成功！");

                        config.sessdata = sessdata;
                        config.save();
                        let user_info = network::get_user_info(&config).await.unwrap();
                        log.info("登录信息有效！");
                        log.info(format!("用户名：{}", user_info.name));
                        log.info(format!("漫币余额：{}", user_info.coin));

                        return;
                    }
                    network::QRStatus::Invalid => {
                        log.error("二维码已失效，请重新运行程序");
                        return;
                    }
                }
            }
        }
    }
}

pub async fn list() {
    let mut log = paris::Logger::new();
    let config = config::Config::load();
    let cache = cache::Cache::load(&config);
    for comic in cache.comics.values() {
        log.info(format!("{} - {}：", comic.id, comic.title));
        let mut episodes = comic.episodes.values().collect::<Vec<_>>();
        episodes.sort_by(|a, b| a.ord.partial_cmp(&b.ord).unwrap());
        let episodes = episodes.iter().map(|e|
            if e.not_downloaded().len() == 0 {
                format!("    {} - {} - {}", e.ord, e.title, "已下载".green())
            } else {
                format!("    {} - {} - {}", e.ord, e.title, "未下载".red())
            }
        ).collect::<Vec<_>>();
        println!("{}", episodes.join("\n"));
    }
}

fn parse_id_or_link(id_or_link: String) -> u32 {
    // 解析id
    // 先判断是不是数字，如果是，直接返回
    if id_or_link.parse::<u32>().is_ok() {
        return id_or_link.parse::<u32>().unwrap();
    }
    // 如果不是数字，判断文中是否包含mc字样，如果包含，则解析出id(mc123456)
    if id_or_link.contains("mc") {
        let id = id_or_link.split("mc").collect::<Vec<&str>>()[1];
        // 从头开始 直到遇到非数字字符为止
        let id = id.chars().take_while(|c| c.is_numeric()).collect::<String>();
        if let Ok(id) = id.parse::<u32>() {
            return id;
        }
    }
    let mut log = paris::Logger::new();
    log.error("指定的id或链接无效！");
    exit(1);
}

pub async fn search(id_or_link: String) {
    let id = parse_id_or_link(id_or_link);
    let mut log = paris::Logger::new();
    let config = config::Config::load();
    let mut comic_info = network::get_comic_info(&config, id).await;
    log.success(format!("漫画标题：{}", comic_info.title.bold()));
    log.success(format!("漫画作者 / 出版社：{}", comic_info.author_name.join(",")));
    log.success(format!("漫画标签：{}", comic_info.styles.join(",")));
    comic_info.ep_list.sort_by(|a, b| a.ord.partial_cmp(&b.ord).unwrap());

    let episodes: Vec<String> = comic_info.ep_list.iter().map(|ep| {
        let ep = ep.to_owned();
        if ep.is_locked {
            format!("    {} - {} - {}", ep.ord, "锁定".red(), ep.title)
        } else {
            format!("    {} - {} - {}", ep.ord, "可用".green(), ep.title)
        }
    }).collect();
    log.success("漫画章节：\n");
    println!("{}", episodes.join("\n"));
}

#[derive(Debug)]
enum Msg {
    Size(usize),
    Halt,
}

async fn run_task(
    config: &Config,
    ep: &EpisodeInfo,
    ep_cache: Option<EpisodeCache>,
    ep_root: &PathBuf,
    statics_sender: &Sender<Msg>,
    halt_receiver: &crossbeam::channel::Receiver<()>,
    bar: &ProgressBar,
) -> Option<()> {
    // 获取某个章节的图片索引
    let ep_cache = if let Some(ep_cache) = ep_cache {
        // ep_cache.paths = indexes.paths;
        // ep_cache.host = indexes.host;
        // ep_cache.sync(&ep_root);
        ep_cache
    } else {
        let indexes = network::get_episode_images(&config, ep.id).await.unwrap();
        let ep_cache = cache::EpisodeCache {
            id: ep.id,
            title: ep.title.to_owned(),
            files: vec![],
            paths: indexes.paths,
            host: indexes.host,
            ord: ep.ord,
        };
        ep_cache.sync(&ep_root);
        ep_cache
    };

    let not_downloaded = ep_cache.not_downloaded();
    for (i, url) in network::get_image_tokens(&config, not_downloaded.clone()).await.unwrap().iter().enumerate() {
        if halt_receiver.try_recv().is_ok() {
            return None;
        }
        let file_name = not_downloaded.get(i).unwrap().split("/").last().unwrap();
        let path = ep_root.join(file_name);
        if let Some(size) = down_to(&config, url.to_owned(), &path).await {
            statics_sender.send(Msg::Size(size)).await.unwrap();
        }
    }
    if not_downloaded.len() == 0 {
        bar.inc(1);
        Some(())
    } else {
        None
    }
}


pub async fn fetch(id_or_link: String, from: f64, to: f64) {
    let id = parse_id_or_link(id_or_link);
    let mut log = paris::Logger::new();
    let config = config::Config::load();
    let comic_info = network::get_comic_info(&config, id).await;
    let cache = cache::Cache::load(&config);
    let cache_root = Path::new(&config.cache_dir);
    let cover_path = &cache_root.join(format!("{}", id)).join("cover.jpg");

    let comic_cache = if let Some(comic) = cache.get_comic(id) {
        let mut comic = comic.clone();
        comic.title = comic_info.title.to_owned();
        comic
    } else {
        // 并没有这个漫画的缓存，则创建一个
        // 保存漫画封面
        cache::ComicCache {
            id,
            title: comic_info.title.to_owned(),
            episodes: HashMap::new(),
        }
    };
    if !cover_path.is_file() {
        if let None = down_to(&config, comic_info.vertical_cover.clone(), cover_path).await {
            log.error("漫画封面下载失败");
            exit(1);
        }
    }
    comic_cache.sync(&cache_root.join(format!("{}", id)));
    // 获取全部可用章节


    let mut ep_list = comic_info.ep_list.clone();
    ep_list.retain(|ep| {
        if ep.is_locked || ep.ord < from {
            false
        } else {
            if to > 0.0 {
                if ep.ord > to {
                    return false;
                }
            }
            if let Some(ep_cache) = comic_cache.get_episode(ep.id) {
                ep_cache.not_downloaded().len() != 0
            } else {
                true
            }
        }
    }
    );
    if ep_list.len() == 0 {
        log.warn("没有需要下载的章节");
        return;
    }

    ep_list.sort_by(|a, b| a.ord.partial_cmp(&b.ord).unwrap());
    log.info("将要下载的漫画章节：\n");
    let episodes: Vec<String> = ep_list.iter().map(|ep| {
        let ep = ep.to_owned();
        format!("    {} - {}", ep.ord, ep.title)
    }).collect();
    println!("{}", episodes.join("\n"));
    // 这里可以多线程

    log.info("启动下载线程...");

    let style = indicatif::ProgressStyle::default_bar()
        .template(
            "[{elapsed_precise}] {bar:40.cyan/blue} {pos:>7}/{len:7} {msg}"
        )
        .progress_chars("##-");

    let bar_overall = ProgressBar::new(ep_list.len() as u64);
    bar_overall.set_style(style.clone());

    let (statics_sender, mut statics_receiver) = tokio::sync::mpsc::channel(10);
    let (halt_sender, halt_receiver) = crossbeam::channel::unbounded();
    ctrlc::set_handler(move || {
        halt_sender.send(()).unwrap();
    }).expect("无法设置 ctrl+c 处理函数");

    let mut tasks = Vec::new();
    for ep in ep_list.iter() {
        let ep_root = cache_root.join(format!("{}", id)).join(format!("{}", ep.id));

        let statics_sender = statics_sender.clone();
        let halt_receiver = halt_receiver.clone();
        let bar = bar_overall.clone();
        let config = config.clone();
        let ep = ep.clone();
        tasks.push(
            tokio::task::spawn(async move {
                loop {
                    let ep_cache = EpisodeCache::load(&ep_root);
                    if halt_receiver.try_recv().is_ok() {
                        break;
                    }
                    if let Some(()) = run_task(
                        &config,
                        &ep,
                        ep_cache,
                        &ep_root,
                        &statics_sender,
                        &halt_receiver,
                        &bar,
                    ).await {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
            })
        )
    }


    let bar = bar_overall.clone();
    tokio::task::spawn(async move {
        let mut last = (chrono::Utc::now(), 0);
        bar.set_message("计算下载速度...");
        loop {
            match statics_receiver.recv().await {
                Some(Msg::Halt) => {
                    break;
                }
                Some(Msg::Size(size)) => {
                    let now = chrono::Utc::now();
                    let duration = now.signed_duration_since(last.0);
                    if duration.num_seconds() >= 1 {
                        let bytes_per_second = size as f64 / duration.num_seconds() as f64;
                        bar.set_message(format!("{} / s", bytes_with_unit(bytes_per_second as u64)));
                        last = (now, size);
                    }
                }
                None => {
                    let now = chrono::Utc::now();
                    let duration = now.signed_duration_since(last.0);
                    if duration.num_seconds() >= 1 {
                        bar.set_message(format!("{} / s", bytes_with_unit(0)));
                        last = (now, 0);
                    }
                }
            }
        }
    });

    for task in tasks {
        task.await.unwrap();
    }
    statics_sender.send(Msg::Halt).await.unwrap();


    bar_overall.finish();
    // 进行清理工作
}

pub fn export(id_or_link: String, from: f64, to: f64, split_episodes: bool, export_dir: Option<&str>, format: String) {
    let mut log = paris::Logger::new();
    let id = parse_id_or_link(id_or_link);
    let config = Config::load();
    let cache = cache::Cache::load(&config);
    if let Some(comic_cache) = cache.get_comic(id) {
        log.info(format!("开始导出漫画：{}", comic_cache.title));
        log.info(format!("导出质量：{}", if let Some(dpi) = config.dpi {
            format!("{}dpi", dpi)
        } else {
            "最佳".to_string()
        }));
        let mut ep_list = comic_cache.episodes.values().collect::<Vec<_>>();
        ep_list.sort_by(|a, b| a.ord.partial_cmp(&b.ord).unwrap());
        ep_list.retain(|ep| {
            if ep.not_downloaded().len() > 0 {
                return false;
            }
            if ep.ord < from {
                return false;
            }
            if to > 0.0 && ep.ord > to {
                return false;
            }
            true
        });
        if ep_list.len() == 0 {
            log.error("没有可以导出的章节");
            return;
        }
        let comic_dir = Path::new(&config.cache_dir).join(format!("{}", id));

        let out_dir = export_dir.unwrap_or(config.default_download_dir.as_str());
        let out_dir = Path::new(out_dir).join(format!("{}", comic_cache.title));
        if !out_dir.exists() || !out_dir.is_dir() {
            std::fs::create_dir_all(&out_dir).unwrap();
        }
        let bar_style = indicatif::ProgressStyle::default_bar()
            .template(
                "[{elapsed_precise}] {bar:40.cyan/blue} {pos:>7}/{len:7} {msg}"
            )
            .progress_chars("##-");
        let bar = ProgressBar::new(ep_list.len() as u64);
        bar.set_style(bar_style);

        if format == "pdf" {
            if split_episodes {
                let mut files = Vec::new();
                log.info("为每一话生成PDF文件...");
                for ep in ep_list {
                    let ep_dir = comic_dir.join(format!("{}", ep.id));
                    let paths = ep.paths.iter().map(|link| {
                        let file_name = link.split('/').last().unwrap();
                        ep_dir.join(file_name)
                    }).collect::<Vec<_>>();

                    let doc = pdf::from_images(paths, ep.title.clone(), format!("{} - {}", ep.ord, ep.title.clone()), config.dpi.clone());
                    let path = ep_dir.join(
                        if let Some(dpi) = config.dpi {
                            format!("{}-dpi.pdf", dpi)
                        } else {
                            "best.pdf".to_string()
                        }
                    );
                    let mut file = std::fs::File::create(&path).unwrap();
                    let mut buf_writer = std::io::BufWriter::new(&mut file);
                    doc.save(&mut buf_writer).unwrap();
                    files.push((path, format!("{}-{}.pdf", ep.ord, ep.title)));
                    bar.inc(1);
                }
                bar.finish();


                // 将每一话的PDF文件分别复制到对应的文件夹中
                for (path, target_name) in files {
                    std::fs::copy(&path, out_dir.join(target_name)).unwrap();
                }
                log.success(format!("漫画导出至：{}", out_dir.display()));
            } else {
                log.loading("生成PDF文件...");
                let mut pdf = None;
                for (i, ep) in ep_list.iter().enumerate() {
                    let ep_dir = comic_dir.join(format!("{}", ep.id));
                    let paths = ep.paths.iter().map(|link| {
                        let file_name = link.split('/').last().unwrap();
                        ep_dir.join(file_name)
                    }).collect::<Vec<_>>();
                    if i == 0 {
                        pdf = Some(pdf::from_images(paths, comic_cache.title.clone(), format!("{} - {}", ep.ord, ep.title.clone()), config.dpi.clone()));
                    } else {
                        pdf = Some(pdf::append(pdf.unwrap(), paths, format!("{} - {}", ep.ord, ep.title.clone()), config.dpi.clone()));
                    }
                }
                log.done();
                log.success("生成PDF文件完成");

                let path = out_dir.join("merged.pdf".to_string());
                let mut file = std::fs::File::create(&path).unwrap();
                let mut buf_writer = std::io::BufWriter::new(&mut file);
                pdf.unwrap().save(&mut buf_writer).unwrap();
                log.success(format!("漫画导出至：{}", out_dir.display()));
            }
        } else if format == "epub" {
            let cover_path = comic_dir.join("cover.jpg");
            let cover = if cover_path.is_file() {
                let file = std::fs::File::open(&cover_path).unwrap();
                let mut buf_reader = std::io::BufReader::new(file);
                let mut buf = Vec::new();
                buf_reader.read_to_end(&mut buf).unwrap();
                Some(buf)
            } else {
                None
            };
            let content_template = r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<body>
<img src="{src}" alt="{alt}" />
</body>
</html>"#;
            let style = "body { margin: 0; padding: 0; } img { width: 100%; height: auto; }";


            if split_episodes {
                let mut epub_files = Vec::new();


                for ep in ep_list {
                    let ep_dir = comic_dir.join(format!("{}", ep.id));
                    let zip = ZipLibrary::new().unwrap();
                    let mut builder = epub_builder::EpubBuilder::new(zip).unwrap();
                    if let Some(cover) = cover.clone() {
                        builder.add_cover_image("images/cover.jpg", cover.as_slice(), "image/jpeg").unwrap();
                    }
                    builder.metadata("title", format!("{} {} - {}", &comic_cache.title, ep.ord, &ep.title)).unwrap();
                    builder.stylesheet(style.as_bytes()).unwrap();
                    for link in &ep.paths {
                        let file_name = link.split('/').last().unwrap();
                        let file_path = ep_dir.join(file_name);
                        let file = std::fs::File::open(&file_path).unwrap();
                        let mut buf_reader = std::io::BufReader::new(file);
                        let mut buf = Vec::new();
                        buf_reader.read_to_end(&mut buf).unwrap();

                        builder.add_resource(format!("images/{}/{}", ep.id, file_name), buf.as_slice(), "image/jpeg").unwrap();
                        builder.add_content(
                            EpubContent::new(format!("{}/{}", ep.id, file_name), content_template.replace("{src}", &format!("./images/{}/{}", ep.id, file_name)).replace("{alt}", file_name).as_bytes())
                        ).unwrap();
                    }
                    let file = File::create(ep_dir.join("epub.epub")).unwrap();
                    let mut buf_writer = std::io::BufWriter::new(file);
                    builder.generate(&mut buf_writer).unwrap();
                    epub_files.push((ep_dir.join("epub.epub"), format!("{}-{}.epub", ep.ord, ep.title)));
                    bar.inc(1);
                }
                bar.finish();
                for (path, target_name) in epub_files {
                    std::fs::copy(&path, out_dir.join(target_name)).unwrap();
                }
                log.success(format!("漫画导出至：{}", out_dir.display()));
            } else {
                let mut builder = epub_builder::EpubBuilder::new(ZipLibrary::new().unwrap()).unwrap();
                builder.stylesheet(style.as_bytes()).unwrap();
                if let Some(cover) = cover.clone() {
                    builder.add_cover_image("images/cover.jpg", cover.as_slice(), "image/jpeg").unwrap();
                }
                builder.metadata("title", format!("{}", &comic_cache.title)).unwrap();
                for ep in ep_list {
                    let ep_dir = comic_dir.join(format!("{}", ep.id));
                    for (i, link) in ep.paths.iter().enumerate() {
                        let file_name = link.split('/').last().unwrap();
                        let file_path = ep_dir.join(file_name);
                        let file = std::fs::File::open(&file_path).unwrap();
                        let mut buf_reader = std::io::BufReader::new(file);
                        let mut buf = Vec::new();
                        buf_reader.read_to_end(&mut buf).unwrap();
                        builder.add_resource(format!("images/{}/{}", ep.id, file_name), buf.as_slice(), "image/jpeg").unwrap();

                        if i == 0 {
                            builder.add_content(
                                EpubContent::new(format!("{}.xhtml", ep.id), content_template.replace("{src}", &format!("./images/{}/{}", ep.id, file_name)).replace("{alt}", link).as_bytes())
                                    .title(format!("{} - {}", ep.ord, ep.title))
                            ).unwrap();
                        } else {
                            builder.add_content(
                                EpubContent::new(format!("{}-{}.xhtml", ep.id, i), content_template.replace("{src}", &format!("./images/{}/{}", ep.id, file_name)).replace("{alt}", link).as_bytes())
                                    .level(2)
                            ).unwrap();
                        }
                    }
                    bar.inc(1);
                }
                bar.finish();


                log.loading("正在生成EPUB文件...");
                let file = File::create(out_dir.join("comic.epub")).unwrap();
                let mut buf_writer = std::io::BufWriter::new(file);
                builder.generate(&mut buf_writer).unwrap();
                log.success(format!("漫画导出至：{}", out_dir.display()));
            }
        }
    } else {
        log.error("在本地缓存中找不到该漫画");
    }
}