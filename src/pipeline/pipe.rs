use anyhow::Result;

use crate::abstraction::{MiddlewareTrait, PipeContextTrait, PipeTrait};

pub struct Pipe<Context: PipeContextTrait> {
    middlewares: Vec<Box<dyn MiddlewareTrait<Context = Context> + Send + Sync>>,
}

impl<Context: PipeContextTrait> Pipe<Context> {
    pub fn new() -> Self {
        Self {
            middlewares: Vec::new(),
        }
    }

    pub fn with<M>(mut self, middleware: M) -> Self
    where
        M: MiddlewareTrait<Context = Context> + Send + Sync + 'static,
    {
        self.middlewares.push(Box::new(middleware));
        self
    }

    async fn send_impl(&self, context: &mut Context) -> Result<()> {
        for middleware in &self.middlewares {
            middleware.send(context).await?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl<Context: PipeContextTrait> PipeTrait for Pipe<Context> {
    type Context = Context;

    async fn send(&self, context: &mut Context) -> Result<()> {
        let cancel_token = context.cancel_token();
        let res = cancel_token
            .run_until_cancelled(self.send_impl(context))
            .await;

        match res {
            Some(res) => res,
            None => anyhow::bail!("Context was cancelled"),
        }
    }
}
