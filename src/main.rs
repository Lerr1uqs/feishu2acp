use std::sync::Arc;

use feishu2acp::{
    adapters::{
        acpx::AcpxCliGateway,
        feishu::{FeishuChannelClient, FeishuLongConnectionRuntime, build_lark_client},
        process::SystemProcessRunner,
        repository::FileConversationRepository,
        shell::SystemShellExecutor,
    },
    application::service::{BridgeService, ServiceDefaults},
    config::AppConfig,
    ports::ChannelRuntime,
};
use tracing::info;
use tracing_appender::non_blocking;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv_override().ok();

    let config = AppConfig::from_env()?;
    if let Some(parent) = config.log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let log_dir = config
        .log_path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let log_file_name = config
        .log_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("feishu2acp.log");
    let file_appender = tracing_appender::rolling::never(&log_dir, log_file_name);
    let (file_writer, _file_guard) = non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(EnvFilter::new(config.tracing_filter.clone()))
        .with(fmt::layer().with_target(false))
        .with(
            fmt::layer()
                .with_ansi(false)
                .with_target(false)
                .with_writer(file_writer),
        )
        .init();

    info!(
        workspace = %config.default_workspace.display(),
        state_path = %config.conversation_store_path.display(),
        log_path = %config.log_path.display(),
        default_agent = %config.default_agent,
        permission_mode = %config.default_permission_mode.as_str(),
        reply_chunk_chars = config.reply_chunk_chars,
        acpx_program = %config.acpx.program,
        acpx_args = config.acpx.args.len(),
        nickname = config.feishu.nickname.as_deref().unwrap_or("-"),
        typing_reaction_emoji = config.feishu.typing_reaction_emoji.as_deref().unwrap_or("-"),
        media_dir = %config.feishu.media_dir.display(),
        max_markdown_bytes = config.feishu.max_markdown_bytes,
        enable_markdown_input = config.feishu.enable_markdown_input,
        enable_markdown_output = config.feishu.enable_markdown_output,
        tracing_filter = %config.tracing_filter,
        "starting feishu2acp"
    );

    let process_runner = Arc::new(SystemProcessRunner);
    let repository = Arc::new(FileConversationRepository::new(
        config.conversation_store_path.clone(),
    ));
    let acpx = Arc::new(AcpxCliGateway::new(
        process_runner.clone(),
        config.acpx.clone(),
    ));
    let shell = Arc::new(SystemShellExecutor::new(
        process_runner,
        config.shell.clone(),
    ));
    let lark_client = build_lark_client(&config.feishu);
    let channel = Arc::new(FeishuChannelClient::new(lark_client.clone(), &config.feishu));
    let runtime = FeishuLongConnectionRuntime::new(lark_client, &config.feishu);
    let service = Arc::new(BridgeService::new(
        channel,
        acpx,
        shell,
        repository,
        ServiceDefaults {
            command_prefix: config.command_prefix,
            default_workspace: config.default_workspace,
            default_agent: config.default_agent,
            default_permission_mode: config.default_permission_mode,
            reply_chunk_chars: config.reply_chunk_chars,
        },
    ));

    runtime.run(service).await?;
    info!("feishu runtime exited");
    Ok(())
}
