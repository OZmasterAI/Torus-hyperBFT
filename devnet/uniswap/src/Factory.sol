// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Pair} from "./Pair.sol";

/// Minimal UniswapV2-style factory (D6, S392). Pairs deploy via CREATE2
/// salted by the sorted token pair, so every pair address is a deterministic
/// function of (factory, token0, token1, Pair init code hash) — the property
/// Uniswap-style pairFor math depends on. Router codes against this exact
/// interface (getPair / createPair).
contract Factory {
    mapping(address => mapping(address => address)) public getPair;
    address[] public allPairs;

    event PairCreated(address indexed token0, address indexed token1, address pair, uint256 allPairsLength);

    function allPairsLength() external view returns (uint256) {
        return allPairs.length;
    }

    function createPair(address tokenA, address tokenB) external returns (address pair) {
        require(tokenA != tokenB, "IDENTICAL_ADDRESSES");
        (address token0, address token1) = tokenA < tokenB ? (tokenA, tokenB) : (tokenB, tokenA);
        require(token0 != address(0), "ZERO_ADDRESS");
        require(getPair[token0][token1] == address(0), "PAIR_EXISTS");
        pair = address(new Pair{salt: keccak256(abi.encodePacked(token0, token1))}());
        Pair(pair).initialize(token0, token1);
        getPair[token0][token1] = pair;
        getPair[token1][token0] = pair;
        allPairs.push(pair);
        emit PairCreated(token0, token1, pair, allPairs.length);
    }
}
