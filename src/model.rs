use crate::edge::{Edge, EdgeIndex};
use crate::node::{Constraint, Node, NodeIndex};
use crate::scenario::{ScenarioGroupCollection, ScenarioIndex};
use crate::solvers::Solver;
use crate::state::{EdgeState, NetworkState, NodeState, ParameterState};
use crate::timestep::{Timestep, Timestepper};
use crate::{parameters, recorders, PywrError};
use std::cmp::Ordering;
use std::time::Instant;

pub struct Model {
    pub(crate) nodes: Vec<Node>,
    pub(crate) edges: Vec<Edge>,
    parameters: Vec<Box<dyn parameters::Parameter>>,
    recorders: Vec<Box<dyn recorders::Recorder>>,
    scenarios: ScenarioGroupCollection,
}

// Required for Python API
unsafe impl Send for Model {}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

impl Model {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            parameters: Vec::new(),
            recorders: Vec::new(),
            scenarios: ScenarioGroupCollection::new(),
        }
    }

    /// Returns the initial state of the network
    pub(crate) fn get_initial_state(&self, scenario_indices: &[ScenarioIndex]) -> Vec<NetworkState> {
        let mut states: Vec<NetworkState> = Vec::new();

        for _scenario_index in scenario_indices {
            let mut state = NetworkState::new();

            for node in &self.nodes {
                let node_state = match node {
                    Node::Input(_n) => NodeState::new_flow_state(),
                    Node::Link(_n) => NodeState::new_flow_state(),
                    Node::Output(_n) => NodeState::new_flow_state(),
                    Node::Storage(n) => NodeState::new_storage_state(n.initial_volume),
                };

                state.push_node_state(node_state);
            }

            for _edge in &self.edges {
                state.push_edge_state(EdgeState::new());
            }

            states.push(state)
        }
        states
    }

    pub fn run(
        &mut self,
        timestepper: Timestepper,
        scenarios: ScenarioGroupCollection,
        solver: &mut Box<dyn Solver>,
    ) -> Result<(), PywrError> {
        let now = Instant::now();

        let timesteps = timestepper.timesteps();
        let scenario_indices = scenarios.scenario_indices();
        // One state per scenario
        let mut current_states = self.get_initial_state(&scenario_indices);

        // Setup the solver
        let mut count = 0;
        solver.setup(self)?;

        // Step a timestep
        for timestep in timesteps.iter() {
            let next_states = self.step(timestep, &scenario_indices, solver, &current_states)?;
            current_states = next_states;
            count += scenario_indices.len();
        }
        println!("speed: {} ts/s", count as f64 / now.elapsed().as_secs_f64());
        // println!("final state: {:?}", initial_state);

        Ok(())
    }

    /// Perform a single timestep with the current state, and return the updated states.
    pub(crate) fn step(
        &mut self,
        timestep: &Timestep,
        scenario_indices: &[ScenarioIndex],
        solver: &mut Box<dyn Solver>,
        current_states: &[NetworkState],
    ) -> Result<Vec<NetworkState>, PywrError> {
        let mut next_states = Vec::with_capacity(current_states.len());

        for scenario_index in scenario_indices.iter() {
            let current_state = match current_states.get(scenario_index.index) {
                Some(s) => s,
                None => return Err(PywrError::ScenarioStateNotFound),
            };
            let pstate = self.compute_parameters(&timestep, &scenario_index, current_state)?;

            let next_state = solver.solve(&self, timestep, current_state, &pstate)?;

            self.save_recorders(&timestep, &scenario_index, &next_state, &pstate)?;

            next_states.push(next_state);
        }

        Ok(next_states)
    }

    fn compute_parameters(
        &self,
        timestep: &Timestep,
        scenario_index: &ScenarioIndex,
        state: &NetworkState,
    ) -> Result<ParameterState, PywrError> {
        let mut parameter_state = ParameterState::with_capacity(self.parameters.len());
        for parameter in &self.parameters {
            let value = parameter.compute(timestep, scenario_index, state, &parameter_state)?;
            parameter_state.push(value);
        }

        Ok(parameter_state)
    }

    fn save_recorders(
        &mut self,
        timestep: &Timestep,
        scenario_index: &ScenarioIndex,
        network_state: &NetworkState,
        parameter_state: &[f64],
    ) -> Result<(), PywrError> {
        for recorder in self.recorders.iter_mut() {
            recorder.save(timestep, scenario_index, network_state, parameter_state)?;
        }
        Ok(())
    }

    /// Get a NodeIndex from a node's name
    pub fn get_node_index(&self, name: &str) -> Result<NodeIndex, PywrError> {
        match self.nodes.iter().find(|&n| n.name() == name) {
            Some(node) => Ok(*node.index()),
            None => Err(PywrError::NodeIndexNotFound),
        }
    }

    /// Get a `ParameterIndex` from a parameter's name
    pub fn get_parameter_index(&self, name: &str) -> Result<parameters::ParameterIndex, PywrError> {
        match self.parameters.iter().position(|p| p.meta().name == name) {
            Some(idx) => Ok(idx),
            None => Err(PywrError::ParameterIndexNotFound),
        }
    }

    /// Get a `RecorderIndex` from a recorder's name
    pub fn get_recorder_index(&self, name: &str) -> Result<recorders::RecorderIndex, PywrError> {
        match self.recorders.iter().position(|r| r.meta().name == name) {
            Some(idx) => Ok(idx),
            None => Err(PywrError::RecorderIndexNotFound),
        }
    }

    /// Add a new Node::Input to the model.
    pub fn add_input_node(&mut self, name: &str) -> Result<NodeIndex, PywrError> {
        // Check for name.
        if let Ok(idx) = self.get_node_index(name) {
            return Err(PywrError::NodeNameAlreadyExists(name.to_string(), idx));
        }

        // Now add the node to the network.
        let node_index = self.nodes.len();
        let node = Node::new_input(&node_index, name);
        self.nodes.push(node);
        Ok(node_index)
    }

    /// Add a new Node::Link to the model.
    pub fn add_link_node(&mut self, name: &str) -> Result<NodeIndex, PywrError> {
        // Check for name.
        if let Ok(idx) = self.get_node_index(name) {
            return Err(PywrError::NodeNameAlreadyExists(name.to_string(), idx));
        }

        // Now add the node to the network.
        let node_index = self.nodes.len();
        let node = Node::new_link(&node_index, name);
        self.nodes.push(node);
        Ok(node_index)
    }

    /// Add a new Node::Link to the model.
    pub fn add_output_node(&mut self, name: &str) -> Result<NodeIndex, PywrError> {
        // Check for name.
        if let Ok(idx) = self.get_node_index(name) {
            return Err(PywrError::NodeNameAlreadyExists(name.to_string(), idx));
        }

        // Now add the node to the network.
        let node_index = self.nodes.len();
        let node = Node::new_output(&node_index, name);
        self.nodes.push(node);
        Ok(node_index)
    }

    /// Add a new Node::Link to the model.
    pub fn add_storage_node(&mut self, name: &str, initial_volume: f64) -> Result<NodeIndex, PywrError> {
        // Check for name.
        if let Ok(idx) = self.get_node_index(name) {
            return Err(PywrError::NodeNameAlreadyExists(name.to_string(), idx));
        }

        // Now add the node to the network.
        let node_index = self.nodes.len();
        let node = Node::new_storage(&node_index, name, initial_volume);
        self.nodes.push(node);
        Ok(node_index)
    }

    /// Add a `parameters::Parameter` to the model
    pub fn add_parameter(
        &mut self,
        parameter: Box<dyn parameters::Parameter>,
    ) -> Result<parameters::ParameterIndex, PywrError> {
        if let Ok(idx) = self.get_parameter_index(&parameter.meta().name) {
            return Err(PywrError::ParameterNameAlreadyExists(
                parameter.meta().name.to_string(),
                idx,
            ));
        }

        let parameter_index = self.parameters.len();
        self.parameters.push(parameter);

        Ok(parameter_index)
    }

    /// Add a `recorders::Recorder` to the model
    pub fn add_recorder(
        &mut self,
        recorder: Box<dyn recorders::Recorder>,
    ) -> Result<recorders::RecorderIndex, PywrError> {
        if let Ok(idx) = self.get_recorder_index(&recorder.meta().name) {
            return Err(PywrError::RecorderNameAlreadyExists(
                recorder.meta().name.to_string(),
                idx,
            ));
        }

        let recorder_index = self.recorders.len();
        self.recorders.push(recorder);
        Ok(recorder_index)
    }

    /// Set a constraint on a node.
    pub(crate) fn set_node_constraint(
        &mut self,
        node_idx: NodeIndex,
        parameter_idx: Option<parameters::ParameterIndex>,
        constraint: Constraint,
    ) -> Result<(), PywrError> {
        if let Some(idx) = parameter_idx {
            if idx >= self.parameters.len() {
                return Err(PywrError::ParameterIndexNotFound);
            }
        }

        match self.nodes.get_mut(node_idx) {
            Some(node) => {
                // Try to add the parameter
                node.set_constraint(parameter_idx, constraint)?;
                Ok(())
            }
            None => Err(PywrError::NodeIndexNotFound),
        }
    }

    /// Set a cost on a node.
    pub(crate) fn set_node_cost(
        &mut self,
        node_idx: NodeIndex,
        parameter_idx: Option<parameters::ParameterIndex>,
    ) -> Result<(), PywrError> {
        if let Some(idx) = parameter_idx {
            if idx >= self.parameters.len() {
                return Err(PywrError::ParameterIndexNotFound);
            }
        }

        match self.nodes.get_mut(node_idx) {
            Some(node) => {
                // Try to add the parameter
                node.set_cost(parameter_idx);
                Ok(())
            }
            None => Err(PywrError::NodeIndexNotFound),
        }
    }

    /// Connect two nodes together
    pub(crate) fn connect_nodes(
        &mut self,
        from_node_index: NodeIndex,
        to_node_index: NodeIndex,
    ) -> Result<EdgeIndex, PywrError> {
        // Next edge index
        // let edge_index = self.edges.len();
        // TODO check the an edge with these indices doesn't already exist.

        // We need to get a mutable reference for each node.
        // The compiler needs to know these are not to the same element. We use split_at_mut to
        // give two mutable slices to the nodes array depending on the ordering of the indexes.
        let (from_node, to_node) = match from_node_index.cmp(&to_node_index) {
            Ordering::Less => {
                if to_node_index > self.nodes.len() {
                    return Err(PywrError::NodeIndexNotFound);
                }
                // Left will contain the "from" node at, and
                // right will contain the "to" node as the first index.
                let (left, right) = self.nodes.split_at_mut(to_node_index);
                (&mut left[from_node_index], &mut right[0])
            }
            Ordering::Equal => return Err(PywrError::InvalidNodeConnection),
            Ordering::Greater => {
                if from_node_index > self.nodes.len() {
                    return Err(PywrError::NodeIndexNotFound);
                }
                // Left will contain the "to" node, and
                // right will contain the "from" node as the first index.
                let (left, right) = self.nodes.split_at_mut(from_node_index);
                (&mut right[0], &mut left[to_node_index])
            }
        };

        // Next edge index
        let edge_index = self.edges.len() as EdgeIndex;
        let edge = from_node.connect(to_node, &edge_index)?;
        self.edges.push(edge);

        Ok(edge_index)
    }

    /// Add a scenario to the model.
    pub fn add_scenario(&mut self, name: &str, size: usize) -> Result<(), PywrError> {
        self.scenarios.add_group(name, size);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metric::Metric;
    use crate::model::Model;
    use crate::node::Constraint;
    use crate::recorders::AssertionRecorder;
    use crate::scenario::{ScenarioGroupCollection, ScenarioIndex};
    use crate::solvers::clp::ClpSolver;
    use crate::solvers::Solver;
    use crate::timestep::Timestepper;
    use float_cmp::approx_eq;
    use ndarray::prelude::*;
    use ndarray::Array2;

    fn default_timestepper() -> Timestepper {
        Timestepper::new("2020-01-01", "2020-01-15", "%Y-%m-%d", 1).unwrap()
    }

    fn default_scenarios() -> ScenarioGroupCollection {
        let mut scenarios = ScenarioGroupCollection::new();
        scenarios.add_group("test-scenario", 10);
        scenarios
    }

    #[test]
    fn test_simple_model() {
        let mut model = Model::new();

        let input_node_idx = model.add_input_node("input").unwrap();
        let link_node_idx = model.add_link_node("link").unwrap();
        let output_node_idx = model.add_output_node("output").unwrap();

        assert_eq!(input_node_idx, 0);
        assert_eq!(link_node_idx, 1);
        assert_eq!(output_node_idx, 2);

        let edge_idx = model.connect_nodes(input_node_idx, link_node_idx).unwrap();
        assert_eq!(edge_idx, 0);
        let edge_idx = model.connect_nodes(link_node_idx, output_node_idx).unwrap();
        assert_eq!(edge_idx, 1);

        // Now assert the internal instructure is as expected.
        if let Node::Input(node) = model.nodes.get(input_node_idx).unwrap() {
            assert_eq!(node.outgoing_edges.len(), 1);
        } else {
            panic!("Incorrect node type returned.")
        };

        if let Node::Link(node) = model.nodes.get(link_node_idx).unwrap() {
            assert_eq!(node.incoming_edges.len(), 1);
            assert_eq!(node.outgoing_edges.len(), 1);
        } else {
            panic!("Incorrect node type returned.")
        };

        if let Node::Output(node) = model.nodes.get(output_node_idx).unwrap() {
            assert_eq!(node.incoming_edges.len(), 1);
        } else {
            panic!("Incorrect node type returned.")
        };
    }

    #[test]
    /// Test the duplicate node names are not permitted.
    fn test_duplicate_node_name() {
        let mut model = Model::new();

        model.add_input_node("my-node").unwrap();
        // Second add with the same name
        let node_idx = model.add_input_node("my-node");
        assert_eq!(
            node_idx,
            Err(PywrError::NodeNameAlreadyExists("my-node".to_string(), 0))
        );
        let node_idx = model.add_link_node("my-node");
        assert_eq!(
            node_idx,
            Err(PywrError::NodeNameAlreadyExists("my-node".to_string(), 0))
        );
        let node_idx = model.add_output_node("my-node");
        assert_eq!(
            node_idx,
            Err(PywrError::NodeNameAlreadyExists("my-node".to_string(), 0))
        );
        let node_idx = model.add_storage_node("my-node", 10.0);
        assert_eq!(
            node_idx,
            Err(PywrError::NodeNameAlreadyExists("my-node".to_string(), 0))
        );
    }

    /// Create a simple test model with three nodes.
    fn simple_model() -> Model {
        let mut model = Model::new();

        let input_node_idx = model.add_input_node("input").unwrap();
        let link_node_idx = model.add_link_node("link").unwrap();
        let output_node_idx = model.add_output_node("output").unwrap();

        model.connect_nodes(input_node_idx, link_node_idx).unwrap();
        model.connect_nodes(link_node_idx, output_node_idx).unwrap();

        let inflow = parameters::VectorParameter::new("inflow", vec![10.0; 366]);
        let inflow_idx = model.add_parameter(Box::new(inflow)).unwrap();
        model
            .set_node_constraint(input_node_idx, Some(inflow_idx), Constraint::MaxFlow)
            .unwrap();

        let base_demand = parameters::ConstantParameter::new("base-demand", 10.0);
        let base_demand_idx = model.add_parameter(Box::new(base_demand)).unwrap();

        let demand_factor = parameters::ConstantParameter::new("demand-factor", 1.2);
        let demand_factor_idx = model.add_parameter(Box::new(demand_factor)).unwrap();

        let total_demand = parameters::AggregatedParameter::new(
            "total-demand",
            vec![base_demand_idx, demand_factor_idx],
            parameters::AggFunc::Product,
        );
        let total_demand_idx = model.add_parameter(Box::new(total_demand)).unwrap();

        model
            .set_node_constraint(output_node_idx, Some(total_demand_idx), Constraint::MaxFlow)
            .unwrap();

        let demand_cost = parameters::ConstantParameter::new("demand-cost", -10.0);
        let demand_cost_idx = model.add_parameter(Box::new(demand_cost)).unwrap();
        model.set_node_cost(output_node_idx, Some(demand_cost_idx)).unwrap();

        model
    }

    /// A test model with a single storage node.
    fn simple_storage_model() -> Model {
        let mut model = Model::new();

        let storage_node_idx = model.add_storage_node("reservoir", 100.0).unwrap();
        let output_node_idx = model.add_output_node("output").unwrap();

        model.connect_nodes(storage_node_idx, output_node_idx).unwrap();

        // Apply demand to the model
        // TODO convenience function for adding a constant constraint.
        let demand = parameters::ConstantParameter::new("demand", 10.0);
        let demand_idx = model.add_parameter(Box::new(demand)).unwrap();
        model
            .set_node_constraint(output_node_idx, Some(demand_idx), Constraint::MaxFlow)
            .unwrap();

        let demand_cost = parameters::ConstantParameter::new("demand-cost", -10.0);
        let demand_cost_idx = model.add_parameter(Box::new(demand_cost)).unwrap();
        model.set_node_cost(output_node_idx, Some(demand_cost_idx)).unwrap();

        let max_volume = parameters::ConstantParameter::new("max-volume", 100.0);
        let max_volume_idx = model.add_parameter(Box::new(max_volume)).unwrap();

        model
            .set_node_constraint(storage_node_idx, Some(max_volume_idx), Constraint::MaxVolume)
            .unwrap();

        model
    }

    #[test]
    /// Test adding a constant parameter to a model.
    fn test_constant_parameter() {
        let mut model = Model::new();
        let node_idx = model.add_input_node("input").unwrap();

        let input_max_flow = parameters::ConstantParameter::new("my-constant", 10.0);
        let param_idx = model.add_parameter(Box::new(input_max_flow)).unwrap();
        assert_eq!(param_idx, 0);
        // assign the new parameter to one of the nodes.
        model
            .set_node_constraint(node_idx, Some(param_idx), Constraint::MaxFlow)
            .unwrap();

        // Try to assign a constraint not defined for particular node type
        assert_eq!(
            model.set_node_constraint(node_idx, Some(param_idx), Constraint::MaxVolume),
            Err(PywrError::StorageConstraintsUndefined)
        )
    }

    #[test]
    fn test_step() {
        let mut model = simple_model();
        let timestepper = default_timestepper();
        let scenarios = default_scenarios();
        let mut solver: Box<dyn Solver> = Box::new(ClpSolver::new());

        solver.setup(&model).unwrap();

        let timesteps = timestepper.timesteps();
        let mut ts_iter = timesteps.iter();
        let scenario_indices = scenarios.scenario_indices();
        let ts = ts_iter.next().unwrap();
        let current_state = model.get_initial_state(&scenario_indices);
        assert_eq!(current_state.len(), scenario_indices.len());

        let next_state = model.step(ts, &scenario_indices, &mut solver, &current_state).unwrap();

        assert_eq!(next_state.len(), scenario_indices.len());

        let output_node_idx = model.get_node_index("output").unwrap();

        let state0 = next_state.get(0).unwrap();
        let output_inflow = state0.get_node_in_flow(output_node_idx).unwrap();
        assert!(approx_eq!(f64, output_inflow, 10.0));
    }

    #[test]
    /// Test running a simple model
    fn test_run() {
        let mut model = simple_model();
        let timestepper = default_timestepper();
        let scenarios = default_scenarios();
        let mut solver: Box<dyn Solver> = Box::new(ClpSolver::new());

        // Set-up assertion for "input" node
        let idx = model.get_node_index("input").unwrap();
        let expected = Array2::from_elem((366, 10), 10.0);
        let recorder = AssertionRecorder::new("input-flow", Metric::NodeOutFlow(idx), expected);
        model.add_recorder(Box::new(recorder)).unwrap();

        let idx = model.get_node_index("link").unwrap();
        let expected = Array2::from_elem((366, 10), 10.0);
        let recorder = AssertionRecorder::new("link-flow", Metric::NodeOutFlow(idx), expected);
        model.add_recorder(Box::new(recorder)).unwrap();

        let idx = model.get_node_index("output").unwrap();
        let expected = Array2::from_elem((366, 10), 10.0);
        let recorder = AssertionRecorder::new("output-flow", Metric::NodeInFlow(idx), expected);
        model.add_recorder(Box::new(recorder)).unwrap();

        let idx = model.get_parameter_index("total-demand").unwrap();
        let expected = Array2::from_elem((366, 10), 12.0);
        let recorder = AssertionRecorder::new("total-demand", Metric::ParameterValue(idx), expected);
        model.add_recorder(Box::new(recorder)).unwrap();

        model.run(timestepper, scenarios, &mut solver).unwrap();
    }

    #[test]
    fn test_run_storage() {
        let mut model = simple_storage_model();
        let timestepper = default_timestepper();
        let scenarios = default_scenarios();
        let mut solver: Box<dyn Solver> = Box::new(ClpSolver::new());

        let idx = model.get_node_index("output").unwrap();
        let expected = array![
            [10.0; 10], [10.0; 10], [10.0; 10], [10.0; 10], [10.0; 10], [10.0; 10], [10.0; 10], [10.0; 10], [10.0; 10],
            [10.0; 10], [0.0; 10], [0.0; 10], [0.0; 10], [0.0; 10], [0.0; 10],
        ];
        let recorder = AssertionRecorder::new("output-flow", Metric::NodeInFlow(idx), expected);
        model.add_recorder(Box::new(recorder)).unwrap();

        let idx = model.get_node_index("reservoir").unwrap();
        let expected = array![
            [90.0; 10], [80.0; 10], [70.0; 10], [60.0; 10], [50.0; 10], [40.0; 10], [30.0; 10], [20.0; 10], [10.0; 10],
            [0.0; 10], [0.0; 10], [0.0; 10], [0.0; 10], [0.0; 10], [0.0; 10],
        ];
        let recorder = AssertionRecorder::new("reservoir-volume", Metric::NodeVolume(idx), expected);
        model.add_recorder(Box::new(recorder)).unwrap();

        model.run(timestepper, scenarios, &mut solver).unwrap();
    }

    #[test]
    /// Test `ScenarioGroupCollection` iteration
    fn test_scenario_iteration() {
        let mut collection = ScenarioGroupCollection::new();
        collection.add_group("Scenarion A", 10);
        collection.add_group("Scenarion B", 2);
        collection.add_group("Scenarion C", 5);

        let scenario_indices = collection.scenario_indices();
        let mut iter = scenario_indices.iter();

        // Test generation of scenario indices
        assert_eq!(
            iter.next(),
            Some(&ScenarioIndex {
                index: 0,
                indices: vec![0, 0, 0]
            })
        );
        assert_eq!(
            iter.next(),
            Some(&ScenarioIndex {
                index: 1,
                indices: vec![0, 0, 1]
            })
        );
        assert_eq!(
            iter.next(),
            Some(&ScenarioIndex {
                index: 2,
                indices: vec![0, 0, 2]
            })
        );
        assert_eq!(
            iter.next(),
            Some(&ScenarioIndex {
                index: 3,
                indices: vec![0, 0, 3]
            })
        );
        assert_eq!(
            iter.next(),
            Some(&ScenarioIndex {
                index: 4,
                indices: vec![0, 0, 4]
            })
        );
        assert_eq!(
            iter.next(),
            Some(&ScenarioIndex {
                index: 5,
                indices: vec![0, 1, 0]
            })
        );
        assert_eq!(
            iter.next(),
            Some(&ScenarioIndex {
                index: 6,
                indices: vec![0, 1, 1]
            })
        );
        assert_eq!(
            iter.next(),
            Some(&ScenarioIndex {
                index: 7,
                indices: vec![0, 1, 2]
            })
        );
        assert_eq!(
            iter.next(),
            Some(&ScenarioIndex {
                index: 8,
                indices: vec![0, 1, 3]
            })
        );
        assert_eq!(
            iter.next(),
            Some(&ScenarioIndex {
                index: 9,
                indices: vec![0, 1, 4]
            })
        );
        assert_eq!(
            iter.next(),
            Some(&ScenarioIndex {
                index: 10,
                indices: vec![1, 0, 0]
            })
        );

        // Test final index
        assert_eq!(
            iter.last(),
            Some(&ScenarioIndex {
                index: 99,
                indices: vec![9, 1, 4]
            })
        );
    }
}