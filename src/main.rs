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
use tracing_subscriber::{EnvFilter, fmt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let config = AppConfig::from_env()?;
    fmt()
        .with_env_filter(EnvFilter::new(config.tracing_filter.clone()))
        .with_target(false)
        .init();

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
    let channel = Arc::new(FeishuChannelClient::new(lark_client.clone()));
    let runtime = FeishuLongConnectionRuntime::new(lark_client);
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
    Ok(())
}
