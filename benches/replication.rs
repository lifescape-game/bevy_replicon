use std::{
    any,
    time::{Duration, Instant},
};

use bevy::prelude::*;
use bevy_replicon::{prelude::*, test_app::ServerTestAppExt};
use criterion::{criterion_group, criterion_main, Criterion};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

#[derive(Clone, Component, Default, Deserialize, Serialize)]
struct UsizeComponent(usize);

#[derive(Component, Clone, Serialize, Deserialize)]
struct StringComponent(String);

impl Default for StringComponent {
    fn default() -> Self {
        Self(".".repeat(60))
    }
}

#[derive(Component, Clone, Serialize, Deserialize)]
struct StructComponent {
    x: u32,
    y: u32,
    b: f32,
    a: f32,
    n: String,
}

impl Default for StructComponent {
    fn default() -> Self {
        Self {
            x: 22u32,
            y: 22u32,
            b: 1.5f32,
            a: 20.0f32,
            n: String::from("abcdef123"),
        }
    }
}

fn replication<C: Component + Default + Serialize + DeserializeOwned + Clone>(c: &mut Criterion) {
    const ENTITIES: usize = 1000;
    const MODULE_PREFIX_LEN: usize = module_path!().len() + 2;

    let mut name = any::type_name::<C>();
    name = &name[MODULE_PREFIX_LEN..];

    for clients in [1, 20] {
        c.bench_function(&format!("{name}, changes send, {clients} client(s)"), |b| {
            b.iter_custom(|iter| {
                let mut elapsed = Duration::ZERO;
                for _ in 0..iter {
                    let mut server_app = create_app::<C>();
                    let mut client_apps = Vec::new();
                    for _ in 0..clients {
                        client_apps.push(create_app::<C>());
                    }

                    for client_app in &mut client_apps {
                        server_app.connect_client(client_app);
                    }

                    server_app
                        .world_mut()
                        .spawn_batch(vec![(Replicated, C::default()); ENTITIES]);

                    let instant = Instant::now();
                    server_app.update();
                    elapsed += instant.elapsed();

                    for client_app in &mut client_apps {
                        server_app.exchange_with_client(client_app);
                        client_app.update();

                        let mut replicated = client_app.world_mut().query::<&Replicated>();
                        assert_eq!(replicated.iter(client_app.world()).count(), ENTITIES);
                    }
                }

                elapsed
            })
        });

        c.bench_function(
            &format!("{name}, mutations send, {clients} client(s)"),
            |b| {
                b.iter_custom(|iter| {
                    let mut server_app = create_app::<C>();
                    let mut client_apps = Vec::new();
                    for _ in 0..clients {
                        client_apps.push(create_app::<C>());
                    }

                    for client_app in &mut client_apps {
                        server_app.connect_client(client_app);
                    }

                    server_app
                        .world_mut()
                        .spawn_batch(vec![(Replicated, C::default()); ENTITIES]);
                    let mut query = server_app.world_mut().query::<&mut C>();

                    server_app.update();
                    for client_app in &mut client_apps {
                        server_app.exchange_with_client(client_app);
                        client_app.update();

                        let mut replicated = client_app.world_mut().query::<&Replicated>();
                        assert_eq!(replicated.iter(client_app.world()).count(), ENTITIES);
                    }

                    let mut elapsed = Duration::ZERO;
                    for _ in 0..iter {
                        for mut component in query.iter_mut(server_app.world_mut()) {
                            component.set_changed();
                        }

                        let instant = Instant::now();
                        server_app.update();
                        elapsed += instant.elapsed();

                        for client_app in &mut client_apps {
                            server_app.exchange_with_client(client_app);
                            client_app.update();

                            let mut replicated = client_app.world_mut().query::<&Replicated>();
                            assert_eq!(replicated.iter(client_app.world()).count(), ENTITIES);
                        }
                    }

                    elapsed
                })
            },
        );
    }

    c.bench_function(&format!("{name}, changes receive"), |b| {
        b.iter_custom(|iter| {
            let mut elapsed = Duration::ZERO;
            for _ in 0..iter {
                let mut server_app = create_app::<C>();
                let mut client_app = create_app::<C>();

                server_app.connect_client(&mut client_app);

                server_app
                    .world_mut()
                    .spawn_batch(vec![(Replicated, C::default()); ENTITIES]);

                server_app.update();
                server_app.exchange_with_client(&mut client_app);

                let instant = Instant::now();
                client_app.update();
                elapsed += instant.elapsed();

                let mut replicated = client_app.world_mut().query::<&Replicated>();
                assert_eq!(replicated.iter(client_app.world()).count(), ENTITIES);
            }

            elapsed
        })
    });

    c.bench_function(&format!("{name}, mutations receive"), |b| {
        b.iter_custom(|iter| {
            let mut server_app = create_app::<C>();
            let mut client_app = create_app::<C>();

            server_app.connect_client(&mut client_app);

            server_app
                .world_mut()
                .spawn_batch(vec![(Replicated, C::default()); ENTITIES]);
            let mut query = server_app.world_mut().query::<&mut C>();

            server_app.update();
            server_app.exchange_with_client(&mut client_app);
            client_app.update();
            let mut replicated = client_app.world_mut().query::<&Replicated>();
            assert_eq!(replicated.iter(client_app.world()).count(), ENTITIES);

            let mut elapsed = Duration::ZERO;
            for _ in 0..iter {
                for mut component in query.iter_mut(server_app.world_mut()) {
                    component.set_changed();
                }

                server_app.update();
                server_app.exchange_with_client(&mut client_app);

                let instant = Instant::now();

                client_app.update();
                elapsed += instant.elapsed();

                let mut replicated = client_app.world_mut().query::<&Replicated>();
                assert_eq!(replicated.iter(client_app.world()).count(), ENTITIES);
            }

            elapsed
        })
    });
}

fn create_app<C: Component + Serialize + DeserializeOwned>() -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        RepliconPlugins.set(ServerPlugin {
            tick_policy: TickPolicy::EveryFrame,
            ..Default::default()
        }),
    ))
    .replicate::<C>()
    .finish();

    app
}

criterion_group!(int_benches, replication::<UsizeComponent>);
criterion_group!(string_benches, replication::<StringComponent>);
criterion_group!(struct_benches, replication::<StructComponent>);

criterion_main!(int_benches, string_benches, struct_benches);
