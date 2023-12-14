#![forbid(unsafe_code)]
use std::{
    any::{Any, TypeId},
    collections::HashMap,
    future::Future,
    sync::Arc,
};

use async_trait::async_trait;
use tokio::{sync::RwLock, task::JoinHandle};

type AnonEventArc = Arc<dyn Any + Send + Sync>;
type AnonHandlerBox = Box<dyn AnonHandlerTrait>;
pub type HandlerResult = ();

pub trait Event: Any + Send + Sync {}
#[async_trait]
pub trait Handler<E: Event>: 'static + Send + Sync {
    async fn run(&self, event: Arc<E>, dispatcher: &dyn Dispatcher);
}

#[async_trait]
pub trait Dispatcher: Send + Sync {
    async fn dispatch_anon(&self, event: AnonEvent);
}

impl dyn Dispatcher {
    pub async fn dispatch<E: Event>(&self, event: Arc<E>) {
        self.dispatch_anon(AnonEvent::from(event)).await
    }
}

#[async_trait]
trait AnonHandlerTrait: Send + Sync {
    async fn run(&self, event: AnonEvent, dispatcher: &dyn Dispatcher);
    fn clone(&self) -> AnonHandlerBox;
}

#[derive(Clone)]
pub struct AnonEvent {
    type_id: TypeId,
    value: AnonEventArc,
}

struct SemiAnonHandler<E: Event>(Arc<dyn Handler<E>>);

pub struct AnonHandler {
    event_id: TypeId,
    value: AnonHandlerBox,
}

#[derive(Clone, Default)]
pub struct Bus {
    registered: Arc<RwLock<HashMap<TypeId, Vec<AnonHandler>>>>,
    running: Arc<RwLock<Vec<JoinHandle<HandlerResult>>>>,
}

impl AnonEvent {
    pub fn deanon<E: Event>(self) -> Option<Arc<E>> {
        self.value.downcast::<E>().ok()
    }
}

impl<E: Event> SemiAnonHandler<E> {
    pub fn new<H: Handler<E>>(handler: Arc<H>) -> Self {
        Self(handler)
    }
}

impl AnonHandler {
    pub fn new<H: Handler<E>, E: Event>(handler: Arc<H>) -> Self {
        Self {
            event_id: TypeId::of::<E>(),
            value: Box::new(SemiAnonHandler::new(handler)),
        }
    }
    pub async fn run(&self, event: AnonEvent, dispatcher: &dyn Dispatcher) {
        let handler = self.value.clone();
        handler.run(event, dispatcher).await;
    }
}

impl Bus {
    pub async fn subscribe<H: Handler<E>, E: Event>(&self, handler: H) {
        let handler = AnonHandler::new(Arc::new(handler));
        self.subscribe_anon(handler).await;
    }
    pub async fn subscribe_shared<H: Handler<E>, E: Event>(&self, handler: Arc<H>) {
        let handler = AnonHandler::new(handler);
        self.subscribe_anon(handler).await;
    }
    pub async fn subscribe_anon(&self, handler: AnonHandler) {
        let registered = &mut self.registered.write().await;
        if let Some(handlers) = registered.get_mut(&handler.event_id) {
            handlers.push(handler);
        } else {
            registered.insert(handler.event_id, vec![handler]);
        }
    }

    pub async fn shutdown(&self) {
        let mut running = self.running.write().await;
        let mut new_running = vec![];
        std::mem::swap(running.as_mut(), &mut new_running);
        drop(running);
        for running in new_running {
            running.await;
        }
    }
}

#[async_trait]
impl Dispatcher for Bus {
    async fn dispatch_anon(&self, event: AnonEvent) {
        let mut new_running = self
            .registered
            .read()
            .await
            .get(&event.type_id)
            .iter()
            .flat_map(|x| x.iter())
            .map(|handler| {
                let handler = handler.clone();
                let dispatcher = self.clone();
                let event = event.clone();
                tokio::spawn(async move { handler.run(event, &dispatcher).await })
            })
            .collect::<Vec<_>>();
        self.running.write().await.append(&mut new_running);
    }
}

impl<E: Event> From<Arc<E>> for AnonEvent {
    fn from(value: Arc<E>) -> Self {
        Self {
            type_id: TypeId::of::<E>(),
            value,
        }
    }
}

impl<E: Event> From<E> for AnonEvent {
    fn from(value: E) -> Self {
        Arc::new(value).into()
    }
}

#[async_trait]
impl<E: Event> AnonHandlerTrait for SemiAnonHandler<E> {
    async fn run(&self, event: AnonEvent, dispatcher: &dyn Dispatcher) {
        if let Some(event) = event.deanon::<E>() {
            self.0.run(event, dispatcher).await
        }
    }

    fn clone(&self) -> AnonHandlerBox {
        Box::new(Self(self.0.clone()))
    }
}

impl Clone for AnonHandler {
    fn clone(&self) -> Self {
        Self {
            event_id: self.event_id,
            value: self.value.clone(),
        }
    }
}

//TODO: doesn't work
#[async_trait]
impl<F, E, Fut> Handler<E> for F
where
    F: Fn(Arc<E>, &dyn Dispatcher) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = HandlerResult> + Send + Sync,
    E: Event,
{
    async fn run(&self, event: Arc<E>, dispatcher: &dyn Dispatcher) {
        (self)(event, dispatcher).await
    }
}

#[cfg(test)]
mod tests {
    use std::{
        assert_eq,
        sync::{
            atomic::{AtomicU8, Ordering},
            Arc,
        },
    };

    use async_trait::async_trait;

    use crate::{AnonEvent, Bus, Dispatcher, Event, Handler};

    struct Hello {
        callback: AtomicU8,
    }
    impl Event for Hello {}

    struct Hello2 {}
    impl Event for Hello2 {}

    struct HelloHandler;
    #[async_trait]
    impl Handler<Hello> for HelloHandler {
        async fn run(&self, event: Arc<Hello>, dispatcher: &dyn Dispatcher) {
            event.callback.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[tokio::test]
    async fn basic() {
        let bus = Bus::default();

        bus.subscribe(HelloHandler).await;

        let event1 = Arc::new(Hello {
            callback: AtomicU8::new(0),
        });
        let event2 = Arc::new(Hello {
            callback: AtomicU8::new(0),
        });

        bus.dispatch_anon(AnonEvent::from(event1.clone())).await;
        bus.dispatch_anon(AnonEvent::from(event2.clone())).await;
        bus.dispatch_anon(AnonEvent::from(event2.clone())).await;

        bus.shutdown().await;

        assert_eq!(event1.callback.load(Ordering::Acquire), 1);
        assert_eq!(event2.callback.load(Ordering::Acquire), 2);
    }
}
