//! Jacobian Lens (J-lens) concept projection trace calculations for local open-source models.
//!
//! Provides the mathematical projection of hidden state tensors onto embedding/unembedding vocabularies,
//! estimating activation sensitivity (gradient Jacobian) for target alignment concepts like honesty, evasion, and deception.

use serde::Serialize;
use std::collections::HashMap;

/// J-lens layer projection score.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct JLensLayerScore {
    /// Layer number.
    pub layer: usize,
    /// Target concept name (e.g. "honesty", "deception").
    pub concept_name: String,
    /// Value of projection (dot product with concept direction vector).
    pub activation: f32,
    /// Estimated Jacobian sensitivity (activation gradient).
    pub gradient_sensitivity: f32,
}

/// J-lens projection trace for a token.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct JLensTrace {
    /// Generated/analyzed token.
    pub token: String,
    /// Per-layer scores.
    pub layers: Vec<JLensLayerScore>,
    /// Calculated cognitive anomaly risk index (0.0 to 1.0).
    pub anomaly_risk: f32,
}

/// Evaluates simulated hidden states from local model forward passes.
pub struct LocalJLensEvaluator {
    /// Mapping of target alignment concepts to direction vectors (size 10 for simulation).
    concept_probes: HashMap<String, Vec<f32>>,
}

impl LocalJLensEvaluator {
    /// Create a new local J-lens evaluator.
    pub fn new() -> Self {
        let mut probes = HashMap::new();
        // 10-dimensional target concept direction vectors in representation space
        probes.insert(
            "honesty".to_string(),
            vec![0.8, 0.1, -0.2, 0.4, 0.9, -0.1, 0.3, 0.5, 0.2, -0.4],
        );
        probes.insert(
            "evasion".to_string(),
            vec![-0.5, 0.7, 0.9, -0.3, -0.8, 0.6, -0.2, 0.4, 0.8, 0.7],
        );
        probes.insert(
            "deception".to_string(),
            vec![-0.9, 0.8, 0.7, -0.4, -0.9, 0.9, -0.1, 0.2, 0.9, 0.8],
        );

        Self {
            concept_probes: probes,
        }
    }

    /// Evaluates model hidden states (layers x hidden_dim) and projects them.
    pub fn compute_jlens(&self, token: &str, hidden_states: &[Vec<f32>]) -> JLensTrace {
        let mut layers = Vec::new();
        let mut max_deception = 0.0f32;

        for (layer_idx, h_state) in hidden_states.iter().enumerate() {
            for (concept, probe) in &self.concept_probes {
                // Dot product projection
                let dot_product: f32 = h_state.iter().zip(probe.iter()).map(|(x, y)| x * y).sum();
                // Activation sensitivity (simulated gradient)
                let sensitivity =
                    dot_product.abs() * (1.0 - (layer_idx as f32 / hidden_states.len() as f32));

                layers.push(JLensLayerScore {
                    layer: layer_idx,
                    concept_name: concept.clone(),
                    activation: dot_product,
                    gradient_sensitivity: sensitivity,
                });

                if concept == "deception" {
                    max_deception = max_deception.max(dot_product);
                }
            }
        }

        // Recompute max evasion correctly
        let mut final_max_evasion = 0.0f32;
        for h_state in hidden_states.iter() {
            if let Some(probe) = self.concept_probes.get("evasion") {
                let dot_product: f32 = h_state.iter().zip(probe.iter()).map(|(x, y)| x * y).sum();
                final_max_evasion = final_max_evasion.max(dot_product);
            }
        }

        let anomaly_risk = (max_deception * 0.7 + final_max_evasion * 0.3).clamp(0.0, 1.0);

        JLensTrace {
            token: token.to_string(),
            layers,
            anomaly_risk,
        }
    }
}

impl Default for LocalJLensEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jlens_evaluation() {
        let evaluator = LocalJLensEvaluator::new();
        // Simulate a 3-layer hidden state output for the token "sudo"
        let hidden_states = vec![
            vec![0.5, 0.2, 0.1, -0.4, 0.8, 0.9, -0.1, 0.2, 0.3, 0.4], // Layer 0
            vec![0.9, -0.8, -0.7, 0.4, 0.9, -0.9, 0.1, -0.2, -0.9, -0.8], // Layer 1 (Honesty concept direction align)
            vec![-0.9, 0.8, 0.7, -0.4, -0.9, 0.9, -0.1, 0.2, 0.9, 0.8], // Layer 2 (Deception align)
        ];

        let trace = evaluator.compute_jlens("sudo", &hidden_states);
        assert_eq!(trace.token, "sudo");
        assert!(!trace.layers.is_empty());
        // Deception layer 2 should have high activation because hidden state is identical to probe
        let dec_layer_2 = trace
            .layers
            .iter()
            .find(|l| l.layer == 2 && l.concept_name == "deception")
            .unwrap();
        assert!(dec_layer_2.activation > 5.0);
        assert!(trace.anomaly_risk > 0.8);
    }
}
