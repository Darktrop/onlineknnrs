use std::collections::HashSet;
use hnsw_rs::knncountry::KnnByCountry;
use failure::Error;
use std::path::Path;
use tonic::{Response, Request, Status};
use hnsw_rs::knnservice::Model;
use crate::knn::{*, knn_server::*};
use dipstick::{AtomicBucket, InputScope};
use metrics_runtime::{Receiver, Sink};
use metrics_runtime::data::Histogram;
use hnsw_rs::embedding_computer::UserEvent;

struct LatencyHistogram {
    sink: Sink,
    histogram: Histogram,
}

struct TimeHandle<'a> {
    start: u64,
    sink: &'a Sink,
    histo: &'a Histogram
}

impl<'a> TimeHandle<'a> {
    fn start(sink: &'a Sink, histo: &'a Histogram) -> TimeHandle<'a> {
        let start = sink.now();
        TimeHandle {
            start,
            sink,
            histo
        }
    }

    fn stop(self) {
        let end = self.sink.now();
        self.histo.record_timing(self.start, end);
    }
}

impl LatencyHistogram {
    fn new(sink: Sink, histogram: Histogram) -> LatencyHistogram{
        LatencyHistogram {
            sink, histogram
        }
    }

    fn record(&self) -> TimeHandle {
        TimeHandle::start(&self.sink, &self.histogram)
    }
}


pub struct KnnController {
    countries: HashSet<String>,
    knn_country:KnnByCountry,
    metrics: AtomicBucket,
    latency_histo: LatencyHistogram,
    model: Model,
}
impl KnnController {
    pub fn new(countries: Vec<String>, metrics: AtomicBucket, receiver: &Receiver, model: Model) -> KnnController {
        let mut sink = receiver.sink();
        let histo = sink.histogram("request.latency");
        let latency_histo = LatencyHistogram::new(sink, histo);
        KnnController {
            metrics,
            latency_histo,
            countries: countries.into_iter().map(|c| c.to_uppercase()).collect(),
            knn_country: KnnByCountry::default(),
            model
        }
    }

    pub fn load<P>(&mut self, index_path: P, extra_item_path: P, models_path: Option<P>) -> Result<(), Error>
        where
            P: AsRef<Path>,
    {
        let indices_path = index_path.as_ref();
        let extra_item_path = extra_item_path.as_ref();
        let models_path = models_path.as_ref().map(|p| p.as_ref());
        self.countries.clone().into_iter().map(move |c| {
            info!("Loading country {}", c);
            let load_result = self.knn_country.load(&c,
                                                    indices_path,
                                                    extra_item_path,
                                                    models_path);
            match &load_result {
                Ok(()) => info!("Done for {}", c),
                Err(e) => error!("error loading {}: {}", c, e.to_string())
            }
            load_result
        }).collect()
    }

    fn build_response(products: Vec<(i64, f32)>) -> KnnResponse{
        let mut response = KnnResponse::default();
        response.products = products.iter().map(|(label, score)| {
            Product {
                product_id: *label,
                score: *score,
                dotproduct: 0f32,
                squared_l2_norm: 0f32
            }
        }).collect();
        response
    }
}
#[tonic::async_trait]
impl Knn for KnnController {
    async fn search(
        &self,
        request: Request<KnnRequest>,
    ) -> Result<Response<KnnResponse>, Status> {
        self.metrics.marker("request.count").mark();
        let handle = self.latency_histo.record();
        let request: KnnRequest = request.into_inner();
        debug!("Received request with country: {}", request.country);
        if let Some(knn_service) = self.knn_country.get_service(&request.country) {

            let events: Vec<UserEvent> = request.user_events.iter().map(|event|
                UserEvent { index: event.partner_id, label: event.product_id, timestamp: event.timestamp as u64, event_type: event.event_type }
            ).collect();
            let result = knn_service.get_closest_items(
                &events,
                request.index_id,
                request.result_count as usize,
                self.model.clone());

            match result {
                Ok(r) => {
                    let response = Response::new(KnnController::build_response(r));
                    handle.stop();
                    Ok(response)
                },
                Err(error) => {
                    handle.stop();
                    Err(Status::internal(error.to_string()))
                }
            }
        } else {
            handle.stop();
            Err(Status::not_found(format!("country {} not available", request.country)))
        }
    }
    async fn multi_search(
        &self,
        _request: Request<KnnRequest>,
    ) -> Result<Response<KnnResponse>, Status> {
        Err(Status::unimplemented(""))
    }
    async fn get_available_countries(
        &self,
        _request: Request<()>,
    ) -> Result<Response<AvailableCountriesResponse>, Status> {
        let countries: Vec<CountryInfo> = self.knn_country.get_countries().into_iter().map(|c| {
            let mut country_info = CountryInfo::default();
            country_info.name = c;
            country_info
        }).collect();
        Ok(Response::new(
            AvailableCountriesResponse {
                countries
            }))
    }
    async fn get_indices_for_country(
        &self,
        _request: Request<IndicesRequest>,
    ) -> Result<Response<IndicesResponse>, Status> {
        Err(Status::unimplemented(""))
    }
    async fn get_indexed_products(
        &self,
        _request: Request<IndexedProductsRequest>,
    ) -> Result<Response<IndexedProductsResponse>, Status> {
        Err(Status::unimplemented(""))
    }
}