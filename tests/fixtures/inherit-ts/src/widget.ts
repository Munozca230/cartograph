import { Component } from 'react';

export class Widget extends Component {
  draw(): void {
    this.describe();
    this.rootHelper();
  }
}
